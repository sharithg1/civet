//! Thin async wrapper around the `git` CLI plus parsers for its output.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use civet_proto::{GitChangeKind, GitFileChange};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::SessionError;

/// Runs git commands with a fixed binary and a predictable environment.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    bin: PathBuf,
}

/// Captured output of a git command that may fail.
#[derive(Debug)]
pub(crate) struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Output of [`Git::output_limited`].
#[derive(Debug)]
pub(crate) struct LimitedOutput {
    /// Exit code; None if killed by a signal (including after `truncated`).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// stdout was longer than the limit; `stdout` holds the first bytes and
    /// git was killed.
    pub truncated: bool,
}

impl Git {
    pub(crate) fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// Run `git -C dir args...`, returning output regardless of exit status.
    pub(crate) async fn output(
        &self,
        dir: &Path,
        args: &[&str],
    ) -> Result<GitOutput, SessionError> {
        self.output_with_stdin(dir, args, None).await
    }

    /// [`Self::output`], writing `stdin` to the command's standard input.
    pub(crate) async fn output_with_stdin(
        &self,
        dir: &Path,
        args: &[&str],
        stdin: Option<Vec<u8>>,
    ) -> Result<GitOutput, SessionError> {
        self.output_env(dir, args, stdin, &[]).await
    }

    /// [`Self::run`] with extra environment variables (e.g. `GIT_INDEX_FILE`).
    pub(crate) async fn run_env(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &Path)],
    ) -> Result<String, SessionError> {
        let out = self.output_env(dir, args, None, env).await?;
        if out.success {
            Ok(out.stdout)
        } else if out.stderr.is_empty() {
            Err(SessionError::Git(format!("git {} failed", args.join(" "))))
        } else {
            Err(SessionError::Git(out.stderr))
        }
    }

    async fn output_env(
        &self,
        dir: &Path,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        env: &[(&str, &Path)],
    ) -> Result<GitOutput, SessionError> {
        let spawn_err = |e: std::io::Error| {
            SessionError::Git(format!("failed to run {}: {e}", self.bin.display()))
        };
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            // Why: inherited, these would point git at another repo than `dir`.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_COMMON_DIR")
            .envs(env.iter().copied())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(spawn_err)?;
        // Written from a task so a large input cannot deadlock against git
        // filling its stdout pipe.
        let writer = child.stdin.take().zip(stdin).map(|(mut pipe, data)| {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = pipe.write_all(&data).await;
            })
        });
        let out = child.wait_with_output().await.map_err(spawn_err)?;
        if let Some(w) = writer {
            let _ = w.await;
        }
        Ok(GitOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }

    /// Run `git -C dir args...`, keeping at most `max_stdout` bytes of stdout:
    /// past that git is killed instead of buffering all of its output (a
    /// diff of a huge file).
    pub(crate) async fn output_limited(
        &self,
        dir: &Path,
        args: &[&str],
        max_stdout: usize,
    ) -> Result<LimitedOutput, SessionError> {
        let failed = |e: std::io::Error| {
            SessionError::Git(format!("failed to run {}: {e}", self.bin.display()))
        };
        let mut child = tokio::process::Command::new(&self.bin)
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .spawn()
            .map_err(failed)?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        // Why: drain stderr on its own so a chatty stderr cannot block git,
        // and so git can be killed below without waiting for stderr's EOF
        // (git blocks writing the stdout nobody reads any more).
        let stderr_task = tokio::spawn(async move {
            let mut err = Vec::new();
            let _ = stderr.read_to_end(&mut err).await;
            err
        });
        let mut out = Vec::new();
        let limit = u64::try_from(max_stdout)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        stdout
            .take(limit)
            .read_to_end(&mut out)
            .await
            .map_err(failed)?;
        let truncated = out.len() > max_stdout;
        if truncated {
            out.truncate(max_stdout);
            let _ = child.start_kill();
        }
        let status = child.wait().await.map_err(failed)?;
        let err = stderr_task.await.unwrap_or_default();
        Ok(LimitedOutput {
            code: status.code(),
            stdout: out,
            stderr: String::from_utf8_lossy(&err).trim().to_owned(),
            truncated,
        })
    }

    /// Run and require success; returns stdout. The error carries git's
    /// trimmed stderr (or the exit status if stderr is empty).
    pub(crate) async fn run(&self, dir: &Path, args: &[&str]) -> Result<String, SessionError> {
        let out = self.output(dir, args).await?;
        if out.success {
            Ok(out.stdout)
        } else if out.stderr.is_empty() {
            Err(SessionError::Git(format!("git {} failed", args.join(" "))))
        } else {
            Err(SessionError::Git(out.stderr))
        }
    }

    /// True if `git -C dir args` exits 0.
    pub(crate) async fn succeeds(&self, dir: &Path, args: &[&str]) -> Result<bool, SessionError> {
        Ok(self.output(dir, args).await?.success)
    }

    /// True if `refs/heads/<branch>` exists.
    pub(crate) async fn branch_exists(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<bool, SessionError> {
        let r = format!("refs/heads/{branch}");
        self.succeeds(dir, &["show-ref", "--verify", "--quiet", &r])
            .await
    }

    /// True if `full_ref` (e.g. `refs/remotes/origin/main`) exists.
    pub(crate) async fn ref_exists(
        &self,
        dir: &Path,
        full_ref: &str,
    ) -> Result<bool, SessionError> {
        self.succeeds(dir, &["show-ref", "--verify", "--quiet", full_ref])
            .await
    }

    /// Validate a branch name with `git check-ref-format --branch`.
    pub(crate) async fn check_branch_name(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<(), SessionError> {
        if branch.is_empty() || branch.starts_with('-') {
            return Err(SessionError::InvalidArgument(format!(
                "invalid branch name {branch:?}"
            )));
        }
        if self
            .succeeds(dir, &["check-ref-format", "--branch", branch])
            .await?
        {
            Ok(())
        } else {
            Err(SessionError::InvalidArgument(format!(
                "invalid branch name {branch:?}"
            )))
        }
    }

    /// Resolve user input `rev` to a commit id, refusing anything git could
    /// read as an option (`git rev-parse --verify --quiet --end-of-options
    /// <rev>^{commit}`). InvalidArgument if it does not name a commit.
    pub(crate) async fn verify_commit(
        &self,
        dir: &Path,
        rev: &str,
    ) -> Result<String, SessionError> {
        let invalid = || SessionError::InvalidArgument(format!("{rev:?} is not a commit"));
        if rev.is_empty() || rev.starts_with('-') || rev.contains(['\0', '\n']) {
            return Err(invalid());
        }
        let spec = format!("{rev}^{{commit}}");
        let out = self
            .output(
                dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &spec,
                ],
            )
            .await?;
        let sha = out.stdout.trim();
        if out.success && !sha.is_empty() {
            Ok(sha.to_owned())
        } else {
            Err(invalid())
        }
    }

    /// If `remote` exists, record `branch.<branch>.remote = <remote>` and
    /// `branch.<branch>.merge = refs/heads/<branch>` (per-branch only).
    /// Ok(false) if there is no such remote.
    pub(crate) async fn set_branch_upstream(
        &self,
        dir: &Path,
        branch: &str,
        remote: &str,
    ) -> Result<bool, SessionError> {
        if !self
            .succeeds(dir, &["remote", "get-url", "--", remote])
            .await?
        {
            return Ok(false);
        }
        let merge = format!("refs/heads/{branch}");
        self.run(
            dir,
            &[
                "config",
                "--local",
                &format!("branch.{branch}.remote"),
                remote,
            ],
        )
        .await?;
        self.run(
            dir,
            &[
                "config",
                "--local",
                &format!("branch.{branch}.merge"),
                &merge,
            ],
        )
        .await?;
        Ok(true)
    }

    /// The repository's default branch: `origin/HEAD`'s target, else a local
    /// `main` or `master`, else the branch checked out in `dir`.
    pub(crate) async fn default_branch(&self, dir: &Path) -> Option<String> {
        if let Ok(out) = self
            .run(
                dir,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
        {
            let name = out.trim();
            if let Some(b) = name.strip_prefix("origin/").filter(|b| !b.is_empty()) {
                return Some(b.to_owned());
            }
        }
        for b in ["main", "master"] {
            if self.branch_exists(dir, b).await.unwrap_or(false) {
                return Some(b.to_owned());
            }
        }
        self.run(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PorcelainWorktree {
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain` output. The first entry is the main
/// worktree (or the bare repository directory).
pub(crate) fn parse_worktree_porcelain(text: &str) -> Vec<PorcelainWorktree> {
    let mut out = Vec::new();
    let mut cur: Option<PorcelainWorktree> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(PorcelainWorktree {
                path: value.to_owned(),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match key {
            "HEAD" => {
                let unborn = value.is_empty() || value.bytes().all(|b| b == b'0');
                w.head = (!unborn).then(|| value.to_owned());
            }
            "branch" => {
                w.branch = Some(
                    value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(value)
                        .to_owned(),
                );
            }
            "bare" => w.bare = true,
            "detached" => w.detached = true,
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

/// A [`GitChangeKind`] from a porcelain v2 `XY` status letter; None for `.`
/// (unchanged) and anything unknown.
fn change_kind(c: u8) -> Option<GitChangeKind> {
    Some(match c {
        b'M' => GitChangeKind::Modified,
        b'T' => GitChangeKind::TypeChanged,
        b'A' => GitChangeKind::Added,
        b'D' => GitChangeKind::Deleted,
        b'R' => GitChangeKind::Renamed,
        b'C' => GitChangeKind::Copied,
        b'U' => GitChangeKind::Conflicted,
        _ => return None,
    })
}

/// `git status --porcelain=v2 -z` sorted into lists (line counts unset).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedStatus {
    pub staged: Vec<GitFileChange>,
    pub unstaged: Vec<GitFileChange>,
    pub untracked: Vec<GitFileChange>,
    pub conflicted: Vec<GitFileChange>,
}

fn change(path: &str, old_path: Option<&str>, kind: GitChangeKind) -> GitFileChange {
    GitFileChange {
        path: path.to_owned(),
        old_path: old_path.map(str::to_owned),
        kind,
        additions: None,
        deletions: None,
    }
}

/// Parse `git status --porcelain=v2 -z [--untracked-files=all]`. Ordinary
/// (`1`) and rename/copy (`2`) records give a staged entry for `X` and an
/// unstaged one for `Y`; the side that says `R`/`C` carries the original
/// path. Unmerged (`u`) records go to `conflicted`, `?` to `untracked`;
/// headers (`#`) and ignored files (`!`) are skipped.
pub(crate) fn parse_status_v2(text: &str) -> ParsedStatus {
    let mut out = ParsedStatus::default();
    let mut tokens = text.split('\0');
    while let Some(rec) = tokens.next() {
        let Some(tag) = rec.as_bytes().first() else {
            continue;
        };
        match tag {
            b'1' | b'2' => {
                // 1 XY sub mH mI mW hH hI path
                // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
                let fields = if *tag == b'1' { 9 } else { 10 };
                let parts: Vec<&str> = rec.splitn(fields, ' ').collect();
                let orig = if *tag == b'2' { tokens.next() } else { None };
                let (Some(xy), Some(path)) = (parts.get(1), parts.get(fields - 1)) else {
                    continue;
                };
                let xy = xy.as_bytes();
                if xy.len() != 2 {
                    continue;
                }
                let side = |c: u8| {
                    change_kind(c).map(|k| {
                        let old = matches!(k, GitChangeKind::Renamed | GitChangeKind::Copied)
                            .then_some(orig)
                            .flatten();
                        change(path, old, k)
                    })
                };
                out.staged.extend(side(xy[0]));
                out.unstaged.extend(side(xy[1]));
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                if let Some(path) = rec.splitn(11, ' ').nth(10) {
                    out.conflicted
                        .push(change(path, None, GitChangeKind::Conflicted));
                }
            }
            b'?' => {
                if let Some(path) = rec.strip_prefix("? ") {
                    out.untracked
                        .push(change(path, None, GitChangeKind::Untracked));
                }
            }
            _ => {}
        }
    }
    out
}

/// Added / deleted lines of one file; None for a binary file (`-`).
pub(crate) type LineCounts = (Option<u64>, Option<u64>);

/// Parse `git diff --numstat -z`: `added\tdeleted\tpath\0`, or for a rename
/// or copy `added\tdeleted\t\0old\0new\0`. Keyed by the (new) path; the
/// first record of a path wins.
pub(crate) fn parse_numstat_z(text: &str) -> HashMap<String, LineCounts> {
    let mut out = HashMap::new();
    let mut tokens = text.split('\0');
    while let Some(rec) = tokens.next() {
        let mut parts = rec.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            let _old = tokens.next();
            match tokens.next() {
                Some(new) => new,
                None => continue,
            }
        } else {
            path
        };
        let count = |s: &str| s.parse::<u64>().ok();
        out.entry(path.to_owned())
            .or_insert((count(add), count(del)));
    }
    out
}

/// Parse `git diff --name-status -z`: `X\0path\0`, or for a rename or copy
/// `R<score>\0old\0new\0` (line counts unset).
pub(crate) fn parse_name_status_z(text: &str) -> Vec<GitFileChange> {
    let mut out = Vec::new();
    let mut tokens = text.split('\0');
    while let Some(status) = tokens.next() {
        let Some(&letter) = status.as_bytes().first() else {
            continue;
        };
        let Some(path) = tokens.next() else { break };
        let Some(kind) = change_kind(letter) else {
            continue;
        };
        if matches!(kind, GitChangeKind::Renamed | GitChangeKind::Copied) {
            let Some(new) = tokens.next() else { break };
            out.push(change(new, Some(path), kind));
        } else {
            out.push(change(path, None, kind));
        }
    }
    out
}

/// Sum of `parse_numstat_z` over all files: None if any file is binary,
/// Some((0, 0)) if there was nothing.
pub(crate) fn numstat_total(text: &str) -> LineCounts {
    let mut adds = 0u64;
    let mut dels = 0u64;
    for (a, d) in parse_numstat_z(text).into_values() {
        let (Some(a), Some(d)) = (a, d) else {
            return (None, None);
        };
        adds = adds.saturating_add(a);
        dels = dels.saturating_add(d);
    }
    (Some(adds), Some(dels))
}

/// Line counts of an untracked file as `git diff --numstat` against
/// /dev/null would report them: None for a binary file (a NUL in the first
/// 8000 bytes, git's heuristic).
pub(crate) fn untracked_line_counts(data: &[u8]) -> LineCounts {
    if data[..data.len().min(8000)].contains(&0) {
        return (None, None);
    }
    let newlines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    let partial = u64::from(data.last().is_some_and(|&b| b != b'\n'));
    (Some(newlines + partial), Some(0))
}

/// A path from a client, relative to the worktree root: not empty, not
/// absolute, no `.`/`..` components, no NUL.
pub(crate) fn check_relative_path(path: &str) -> Result<(), SessionError> {
    let ok = !path.is_empty()
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(SessionError::InvalidArgument(format!(
            "{path:?} is not a path inside the worktree"
        )))
    }
}

/// A pathspec that matches `path` literally (no globs or magic).
pub(crate) fn literal_pathspec(path: &str) -> String {
    format!(":(literal){path}")
}

/// Canonicalize if the path exists, else return it unchanged.
pub(crate) fn canonical_or_same(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Canonicalize the longest existing prefix of `p` and append the rest.
/// Why: a worktree's id is its canonical path; once its directory is deleted
/// behind git's back, plain canonicalization falls back to the raw path git
/// recorded (e.g. `/var/...` instead of `/private/var/...`), so the id would
/// change and the client could no longer remove it by the id it holds.
pub(crate) fn canonical_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut rest = Vec::new();
    let mut cur = p;
    while let Some(parent) = cur.parent() {
        if let Some(name) = cur.file_name() {
            rest.push(name.to_owned());
        }
        if let Ok(mut c) = std::fs::canonicalize(parent) {
            c.extend(rest.iter().rev());
            return c;
        }
        cur = parent;
    }
    p.to_path_buf()
}

/// Lossy path → string.
pub(crate) fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// RepoId: hex of the first 8 bytes of SHA-256(canonical common git dir).
pub(crate) fn repo_id_for(common_dir: &Path) -> String {
    let digest = Sha256::digest(path_string(common_dir).as_bytes());
    hex::encode(&digest[..8])
}

/// Directory name for a branch in the default worktree layout.
pub(crate) fn branch_dir_name(branch: &str) -> String {
    branch.replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain() {
        let text = "worktree /r/main\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD 2222222222222222222222222222222222222222\ndetached\nlocked reason here\n\nworktree /r/new\nHEAD 0000000000000000000000000000000000000000\nbranch refs/heads/feat/x\nprunable gitdir file points to non-existent location\n\n";
        let w = parse_worktree_porcelain(text);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].path, "/r/main");
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[0].head.is_some());
        assert!(w[1].detached && w[1].locked && w[1].branch.is_none());
        assert_eq!(w[2].branch.as_deref(), Some("feat/x"));
        assert_eq!(w[2].head, None);
        assert!(w[2].prunable);

        let bare = parse_worktree_porcelain("worktree /r/x.git\nbare\n");
        assert_eq!(bare.len(), 1);
        assert!(bare[0].bare);
    }

    #[test]
    fn status_v2_records() {
        let text = [
            "# branch.oid 1111111111111111111111111111111111111111",
            "1 M. N... 100644 100644 100644 aaaa bbbb src/staged.rs",
            "1 .M N... 100644 100644 100644 aaaa aaaa src/with space.rs",
            "1 MD N... 100644 100644 000000 aaaa bbbb both.txt",
            "1 A. N... 000000 100644 100644 0000 bbbb added.txt",
            "1 .A N... 000000 000000 100644 0000 0000 intent.txt",
            "1 .T N... 100644 100644 120000 aaaa aaaa link",
            "2 RM N... 100644 100644 100644 aaaa aaaa R87 new name.rs",
            "old name.rs",
            "2 C. N... 100644 100644 100644 aaaa aaaa C100 copy.rs",
            "orig.rs",
            "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflict.rs",
            "? untracked dir/file.txt",
            "! ignored.log",
            "",
        ]
        .join("\0");
        let s = parse_status_v2(&text);
        fn summary(v: &[GitFileChange]) -> Vec<(&str, Option<&str>, GitChangeKind)> {
            v.iter()
                .map(|c| (c.path.as_str(), c.old_path.as_deref(), c.kind))
                .collect()
        }
        use GitChangeKind::*;
        assert_eq!(
            summary(&s.staged),
            vec![
                ("src/staged.rs", None, Modified),
                ("both.txt", None, Modified),
                ("added.txt", None, Added),
                ("new name.rs", Some("old name.rs"), Renamed),
                ("copy.rs", Some("orig.rs"), Copied),
            ]
        );
        assert_eq!(
            summary(&s.unstaged),
            vec![
                ("src/with space.rs", None, Modified),
                ("both.txt", None, Deleted),
                ("intent.txt", None, Added),
                ("link", None, TypeChanged),
                ("new name.rs", None, Modified),
            ]
        );
        assert_eq!(
            summary(&s.conflicted),
            vec![("conflict.rs", None, Conflicted)]
        );
        assert_eq!(
            summary(&s.untracked),
            vec![("untracked dir/file.txt", None, Untracked)]
        );
        assert_eq!(parse_status_v2(""), ParsedStatus::default());
    }

    #[test]
    fn status_v2_worktree_rename() {
        // An intent-to-add file renamed in the working tree (`.R`).
        let s = parse_status_v2("2 .R N... 100644 100644 100644 aaaa aaaa R100 b.rs\0a.rs\0");
        assert!(s.staged.is_empty());
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.unstaged[0].path, "b.rs");
        assert_eq!(s.unstaged[0].old_path.as_deref(), Some("a.rs"));
    }

    #[test]
    fn numstat_records() {
        let text = [
            "3\t1\tsrc/a.rs",
            "-\t-\timg.png",
            "5\t0\t",
            "old.rs",
            "new.rs",
            "0\t0\ttab\tname",
            "",
        ]
        .join("\0");
        let m = parse_numstat_z(&text);
        assert_eq!(m.len(), 4);
        assert_eq!(m["src/a.rs"], (Some(3), Some(1)));
        assert_eq!(m["img.png"], (None, None));
        assert_eq!(m["new.rs"], (Some(5), Some(0)));
        assert_eq!(m["tab\tname"], (Some(0), Some(0)));
        assert!(parse_numstat_z("").is_empty());

        assert_eq!(numstat_total(""), (Some(0), Some(0)));
        assert_eq!(numstat_total("3\t1\ta\0"), (Some(3), Some(1)));
        assert_eq!(numstat_total("3\t1\ta\0-\t-\tb\0"), (None, None));
        let huge = format!("{}\t{}\ta\0{}\t1\tb\0", u64::MAX, u64::MAX, u64::MAX);
        assert_eq!(numstat_total(&huge), (Some(u64::MAX), Some(u64::MAX)));
    }

    #[test]
    fn name_status_records() {
        let text = [
            "M", "src/a.rs", "A", "new.txt", "R087", "old.rs", "moved.rs", "D", "gone", "X", "odd",
            "T", "link", "",
        ]
        .join("\0");
        let got = parse_name_status_z(&text);
        assert_eq!(
            got,
            vec![
                change("src/a.rs", None, GitChangeKind::Modified),
                change("new.txt", None, GitChangeKind::Added),
                change("moved.rs", Some("old.rs"), GitChangeKind::Renamed),
                change("gone", None, GitChangeKind::Deleted),
                change("link", None, GitChangeKind::TypeChanged),
            ]
        );
        assert!(parse_name_status_z("").is_empty());
        // Cut short: what was complete is kept.
        assert_eq!(parse_name_status_z("M\0a\0R100\0b").len(), 1);
    }

    #[test]
    fn untracked_counts() {
        assert_eq!(untracked_line_counts(b""), (Some(0), Some(0)));
        assert_eq!(untracked_line_counts(b"a\nb\n"), (Some(2), Some(0)));
        assert_eq!(untracked_line_counts(b"a\nb"), (Some(2), Some(0)));
        assert_eq!(untracked_line_counts(b"\x89PNG\0\0"), (None, None));
    }

    #[test]
    fn relative_paths() {
        for ok in ["a", "src/a.rs", "dir with space/x", "a/./b"] {
            assert!(check_relative_path(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "./a", ".", "a\0b"] {
            assert!(check_relative_path(bad).is_err(), "{bad:?}");
        }
        assert_eq!(literal_pathspec("*.rs"), ":(literal)*.rs");
    }

    #[test]
    fn ids_and_names() {
        let id = repo_id_for(Path::new("/tmp/x/.git"));
        assert_eq!(id.len(), 16);
        assert_eq!(id, repo_id_for(Path::new("/tmp/x/.git")));
        assert_ne!(id, repo_id_for(Path::new("/tmp/y/.git")));
        assert_eq!(branch_dir_name("feat/a/b"), "feat-a-b");
    }

    #[test]
    fn lenient_canonical_path_of_a_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canon_real = std::fs::canonicalize(&real).unwrap();
        // Existing: same as canonicalize.
        assert_eq!(canonical_lenient(&link), canon_real);
        // Missing below a symlink: the existing prefix is still resolved.
        assert_eq!(
            canonical_lenient(&link.join("gone/deeper")),
            canon_real.join("gone/deeper")
        );
    }
}
