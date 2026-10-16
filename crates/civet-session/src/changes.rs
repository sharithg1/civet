//! A worktree's uncommitted changes: `git status` with line counts, and one
//! file's diff (right panel → Changes).

use std::path::Path;

use civet_proto::{
    GitDiffInfo, GitFileChange, GitStatusInfo, MAX_GIT_DIFF_BYTES, MAX_GIT_DIFF_LINES,
    MAX_GIT_STATUS_ENTRIES,
};

use crate::SessionError;
use crate::git::{
    Git, LimitedOutput, LineCounts, check_relative_path, literal_pathspec, numstat_total,
    parse_numstat_z, parse_status_v2, untracked_line_counts,
};

/// Untracked files whose lines are counted per status (the rest show no
/// counts); each read only if at most [`MAX_GIT_DIFF_BYTES`].
const MAX_COUNTED_UNTRACKED: usize = 500;

/// numstat output is a few bytes per file; anything near this is pathological.
const MAX_NUMSTAT_BYTES: usize = 4 * 1024 * 1024;

/// `git status` of the worktree at `dir`, with `git diff --numstat` line
/// counts for staged and unstaged entries and counted lines for untracked
/// files. Uses `--no-optional-locks` so polling never contends with an
/// agent's own git commands for `index.lock`.
pub(crate) async fn status(git: &Git, dir: &Path) -> Result<GitStatusInfo, SessionError> {
    let (status, unstaged, staged) = tokio::join!(
        git.run(
            dir,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
            ],
        ),
        git.run(
            dir,
            &[
                "--no-optional-locks",
                "diff",
                "--numstat",
                "-z",
                "--no-ext-diff"
            ],
        ),
        git.run(
            dir,
            &[
                "--no-optional-locks",
                "diff",
                "--cached",
                "--numstat",
                "-z",
                "-M",
                "--no-ext-diff",
            ],
        ),
    );
    let mut parsed = parse_status_v2(&status?);
    let unstaged_counts = parse_numstat_z(&unstaged?);
    let staged_counts = parse_numstat_z(&staged?);
    let fill = |list: &mut Vec<GitFileChange>,
                counts: &std::collections::HashMap<_, LineCounts>| {
        for c in list {
            if let Some(&(a, d)) = counts.get(&c.path) {
                c.additions = a;
                c.deletions = d;
            }
        }
    };
    fill(&mut parsed.staged, &staged_counts);
    fill(&mut parsed.unstaged, &unstaged_counts);
    fill(&mut parsed.conflicted, &unstaged_counts);

    let mut lists = [
        &mut parsed.conflicted,
        &mut parsed.staged,
        &mut parsed.unstaged,
        &mut parsed.untracked,
    ];
    let mut budget = MAX_GIT_STATUS_ENTRIES;
    let mut truncated = false;
    for list in &mut lists {
        list.sort_by(|a, b| a.path.cmp(&b.path));
        if list.len() > budget {
            list.truncate(budget);
            truncated = true;
        }
        budget -= list.len();
    }

    let root = dir.to_path_buf();
    let mut untracked = std::mem::take(&mut parsed.untracked);
    untracked = tokio::task::spawn_blocking(move || {
        for c in untracked.iter_mut().take(MAX_COUNTED_UNTRACKED) {
            (c.additions, c.deletions) = count_untracked(&root.join(&c.path));
        }
        untracked
    })
    .await
    .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;

    Ok(GitStatusInfo {
        staged: parsed.staged,
        unstaged: parsed.unstaged,
        untracked,
        conflicted: parsed.conflicted,
        truncated,
    })
}

/// Lines of an untracked regular file of at most [`MAX_GIT_DIFF_BYTES`]
/// (anything else: no counts).
fn count_untracked(path: &Path) -> LineCounts {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_GIT_DIFF_BYTES as u64 => {}
        _ => return (None, None),
    }
    match std::fs::read(path) {
        Ok(data) => untracked_line_counts(&data),
        Err(_) => (None, None),
    }
}

/// One file's diff in the worktree at `dir`: index vs. HEAD if `staged`,
/// else working tree vs. index; a path the index does not know is diffed
/// against /dev/null (`git diff --no-index`). With `old_path`, the pair is
/// diffed with rename detection. Binary files and patches over
/// [`MAX_GIT_DIFF_LINES`] changed lines (checked with `--numstat` first) or
/// [`MAX_GIT_DIFF_BYTES`] come back flagged with an empty `diff`.
pub(crate) async fn diff(
    git: &Git,
    dir: &Path,
    path: &str,
    old_path: Option<&str>,
    staged: bool,
    context_lines: u32,
) -> Result<GitDiffInfo, SessionError> {
    check_relative_path(path)?;
    if let Some(old) = old_path {
        check_relative_path(old)?;
    }
    let untracked = !staged && old_path.is_none() && !is_tracked(git, dir, path).await?;
    if untracked {
        check_untracked_file(dir, path)?;
    }

    let mut args: Vec<String> = ["--no-optional-locks", "diff", "--no-color", "--no-ext-diff"]
        .map(String::from)
        .to_vec();
    let mut paths: Vec<String> = Vec::new();
    if untracked {
        args.push("--no-index".into());
        paths.extend(["/dev/null".to_owned(), path.to_owned()]);
    } else {
        if staged {
            args.push("--cached".into());
        }
        args.push("-M".into());
        paths.extend(old_path.map(literal_pathspec));
        paths.push(literal_pathspec(path));
    }
    let with = |extra: &[&str]| {
        let mut a = args.clone();
        a.extend(extra.iter().map(|s| s.to_string()));
        a.push("--".into());
        a.extend(paths.iter().cloned());
        a
    };

    let mut info = GitDiffInfo {
        path: path.to_owned(),
        binary: false,
        too_large: false,
        additions: None,
        deletions: None,
        diff: String::new(),
    };
    let numstat = run_diff(
        git,
        dir,
        &with(&["--numstat", "-z"]),
        untracked,
        MAX_NUMSTAT_BYTES,
    )
    .await?;
    if numstat.truncated {
        info.too_large = true;
        return Ok(info);
    }
    let (adds, dels) = numstat_total(&String::from_utf8_lossy(&numstat.stdout));
    let (Some(adds), Some(dels)) = (adds, dels) else {
        info.binary = true;
        return Ok(info);
    };
    info.additions = Some(adds);
    info.deletions = Some(dels);
    if adds + dels > MAX_GIT_DIFF_LINES {
        info.too_large = true;
        return Ok(info);
    }
    let unified = format!("-U{context_lines}");
    let patch = run_diff(git, dir, &with(&[&unified]), untracked, MAX_GIT_DIFF_BYTES).await?;
    if patch.truncated {
        info.too_large = true;
        return Ok(info);
    }
    info.diff = String::from_utf8_lossy(&patch.stdout).into_owned();
    Ok(info)
}

/// Run a `git diff`, keeping at most `max` bytes of output (`truncated`
/// past that). `no_index`: exit code 1 means "files differ", not failure.
async fn run_diff(
    git: &Git,
    dir: &Path,
    args: &[String],
    no_index: bool,
    max: usize,
) -> Result<LimitedOutput, SessionError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git.output_limited(dir, &args, max).await?;
    if out.truncated || out.code == Some(0) || (no_index && out.code == Some(1)) {
        Ok(out)
    } else if out.stderr.is_empty() {
        Err(SessionError::Git(format!("git {} failed", args.join(" "))))
    } else {
        Err(SessionError::Git(out.stderr))
    }
}

/// True if the index has an entry for `path`.
async fn is_tracked(git: &Git, dir: &Path, path: &str) -> Result<bool, SessionError> {
    let out = git
        .run(
            dir,
            &[
                "--no-optional-locks",
                "ls-files",
                "-z",
                "--cached",
                "--",
                &literal_pathspec(path),
            ],
        )
        .await?;
    Ok(!out.is_empty())
}

/// An untracked path is read straight from disk: it must be a file or
/// symlink whose directory is inside the worktree (no escape through a
/// symlinked parent).
fn check_untracked_file(dir: &Path, path: &str) -> Result<(), SessionError> {
    let full = dir.join(path);
    let meta = std::fs::symlink_metadata(&full)
        .map_err(|_| SessionError::NotFound(format!("{path} in {}", dir.display())))?;
    if meta.is_dir() {
        return Err(SessionError::InvalidArgument(format!(
            "{path} is a directory (a nested repository?)"
        )));
    }
    let root = std::fs::canonicalize(dir)?;
    let parent = full
        .parent()
        .map(std::fs::canonicalize)
        .transpose()?
        .unwrap_or_else(|| root.clone());
    if !parent.starts_with(&root) {
        return Err(SessionError::InvalidArgument(format!(
            "{path} is outside the worktree"
        )));
    }
    Ok(())
}
