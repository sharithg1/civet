//! Git write operations on a worktree (right panel → Changes): stage,
//! unstage, discard (undoable), commit, conflict resolution, push / pull /
//! fetch, undo last commit and stash, plus the branch summary the tab's
//! header shows.
//!
//! Agents may run git in the same worktree at the same time, so every
//! operation names its paths explicitly (`--literal-pathspecs`, paths fed on
//! stdin) and holds `index.lock` only for git's own command; a command that
//! finds the lock taken is retried briefly.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use civet_proto::{
    ConflictSide, GitAction, GitBlobRef, GitBranchInfo, GitDiscardEntry, GitDiscardSnapshot,
};

use crate::SessionError;
use crate::git::{Git, GitOutput, check_relative_path};

/// push / pull / fetch give up after this (a remote that never answers).
const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);

/// Tries of a command that found `index.lock` taken (an agent's git).
const LOCK_TRIES: u32 = 4;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Message of the entries [`GitAction::StashAll`] makes.
const STASH_MESSAGE: &str = "Stashed from Civet";

const MODE_FILE: u32 = 0o100644;
const MODE_EXEC: u32 = 0o100755;
const MODE_SYMLINK: u32 = 0o120000;

/// What a successful [`run`] produced.
pub(crate) enum ActionOutcome {
    Done,
    Discarded(GitDiscardSnapshot),
}

/// Run `action` in the worktree at `dir`.
pub(crate) async fn run(
    git: &Git,
    dir: &Path,
    action: GitAction,
) -> Result<ActionOutcome, SessionError> {
    let checked = |paths: &[String]| action_paths(dir, paths);
    match action {
        GitAction::Stage { paths } => {
            write(git, dir, &["add", "-A"], &checked(&paths)?).await?;
        }
        GitAction::Unstage { paths } => unstage(git, dir, &checked(&paths)?).await?,
        GitAction::Discard { paths, staged } => {
            return discard(git, dir, &checked(&paths)?, staged)
                .await
                .map(ActionOutcome::Discarded);
        }
        GitAction::RestoreDiscarded(snapshot) => restore(git, dir, &snapshot).await?,
        GitAction::Commit {
            message,
            amend,
            all,
        } => commit(git, dir, &message, amend, all).await?,
        GitAction::AcceptConflict { paths, side } => {
            accept_conflict(git, dir, &checked(&paths)?, side).await?
        }
        GitAction::Push => push(git, dir).await?,
        GitAction::Pull => pull(git, dir).await?,
        GitAction::Fetch => {
            network(git, dir, &["fetch"]).await?;
        }
        GitAction::UndoCommit { head } => undo_commit(git, dir, &head).await?,
        GitAction::StashAll => stash_all(git, dir).await?,
        GitAction::StashPop => stash_pop(git, dir).await?,
    }
    Ok(ActionOutcome::Done)
}

// ----- branch summary ---------------------------------------------------------

/// The worktree's branch, upstream, ahead / behind and stash count. Read
/// only (`--no-optional-locks`).
pub(crate) async fn branch(git: &Git, dir: &Path) -> Result<GitBranchInfo, SessionError> {
    const RO: &str = "--no-optional-locks";
    let (branch, head, remotes, upstream, parent) = tokio::join!(
        git.output(dir, &[RO, "symbolic-ref", "-q", "--short", "HEAD"]),
        git.output(dir, &[RO, "rev-parse", "-q", "--verify", "HEAD^{commit}"]),
        git.output(dir, &[RO, "remote"]),
        git.output(
            dir,
            &[
                RO,
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}"
            ]
        ),
        git.output(dir, &[RO, "rev-parse", "-q", "--verify", "HEAD^1"]),
    );
    let ok_line = |o: GitOutput| {
        (o.success && !o.stdout.trim().is_empty()).then(|| o.stdout.trim().to_owned())
    };
    let branch = ok_line(branch?);
    let head = ok_line(head?);
    let has_remote = !remotes?.stdout.trim().is_empty();
    let upstream = ok_line(upstream?);
    let has_parent = ok_line(parent?).is_some();

    let (mut ahead, mut behind) = (0, 0);
    if let (Some(_), Some(_)) = (&upstream, &head) {
        let counts = git
            .run(
                dir,
                &[
                    "--no-optional-locks",
                    "rev-list",
                    "--left-right",
                    "--count",
                    "HEAD...@{upstream}",
                ],
            )
            .await?;
        let mut it = counts
            .split_whitespace()
            .map(|n| n.parse::<u64>().unwrap_or(0));
        ahead = it.next().unwrap_or(0);
        behind = it.next().unwrap_or(0);
    }
    // Pushed: on the upstream, or on any remote branch (a new branch still
    // sitting on main's pushed commits).
    let pushed = head.is_some()
        && ((upstream.is_some() && ahead == 0)
            || !git
                .run(
                    dir,
                    &[
                        "--no-optional-locks",
                        "for-each-ref",
                        "--count=1",
                        "--contains=HEAD",
                        "--format=%(refname)",
                        "refs/remotes",
                    ],
                )
                .await?
                .trim()
                .is_empty());
    let stashes = branch_stashes(git, dir, branch.as_deref()).await?.len() as u64;
    Ok(GitBranchInfo {
        branch,
        can_undo_commit: head.is_some() && has_parent && !pushed,
        head,
        upstream,
        ahead,
        behind,
        stashes,
        has_remote,
    })
}

// ----- stage / unstage ------------------------------------------------------------

async fn unstage(git: &Git, dir: &Path, paths: &[String]) -> Result<(), SessionError> {
    if has_head(git, dir).await? {
        write(git, dir, &["reset", "-q"], paths).await
    } else {
        // Unborn branch: nothing to reset to, so drop the entries.
        write(
            git,
            dir,
            &["rm", "--cached", "-q", "-r", "--ignore-unmatch"],
            paths,
        )
        .await
    }
}

// ----- discard / restore --------------------------------------------------------

/// An index entry from `git ls-files -s`.
struct IndexEntry {
    blob: GitBlobRef,
    stage: u32,
}

async fn discard(
    git: &Git,
    dir: &Path,
    paths: &[String],
    staged: bool,
) -> Result<GitDiscardSnapshot, SessionError> {
    if paths.iter().any(|p| p.contains('\n')) {
        return Err(SessionError::InvalidArgument(
            "paths with newlines can't be discarded".into(),
        ));
    }
    let root = std::fs::canonicalize(dir)?;
    let index = index_entries(git, dir, paths).await?;
    if let Some((p, _)) = index.iter().find(|(_, e)| e.stage != 0) {
        return Err(SessionError::InvalidArgument(format!(
            "{p} has merge conflicts; resolve them instead of discarding"
        )));
    }
    let head = has_head(git, dir).await?;
    let in_head = if staged && head {
        head_paths(git, dir, paths).await?
    } else {
        HashSet::new()
    };

    // Snapshot every working-tree file first: nothing is touched until all
    // of them are in the object database.
    let files = snapshot_files(git, dir, &root, paths).await?;

    let mut entries = Vec::new();
    let mut restore = Vec::new();
    let mut delete = Vec::new();
    for p in paths {
        let idx = index.get(p).map(|e| e.blob.clone());
        let file = files.get(p).cloned();
        let tracked = idx.is_some() || in_head.contains(p);
        if tracked {
            restore.push(p.clone());
        } else if file.is_some() {
            delete.push(p.clone());
        } else {
            continue;
        }
        entries.push(GitDiscardEntry {
            path: p.clone(),
            file,
            restore_index: staged && tracked,
            index: if staged { idx } else { None },
            untracked: !tracked,
        });
    }

    if !restore.is_empty() {
        if !staged {
            write(git, dir, &["restore", "--worktree"], &restore).await?;
        } else if head {
            // No-overlay: a path HEAD lacks leaves the index and the disk.
            write(
                git,
                dir,
                &["restore", "--source=HEAD", "--staged", "--worktree"],
                &restore,
            )
            .await?;
        } else {
            write(git, dir, &["rm", "--cached", "-q", "-r"], &restore).await?;
            delete.extend(restore);
        }
    }
    for p in &delete {
        remove_file(&root, p)?;
    }
    Ok(GitDiscardSnapshot { entries })
}

/// Put back what [`discard`] overwrote: index entries first, then files.
/// Refused if any path changed since the discard (an agent kept editing):
/// that newer content has no snapshot, so overwriting it would lose it.
async fn restore(git: &Git, dir: &Path, snapshot: &GitDiscardSnapshot) -> Result<(), SessionError> {
    let root = std::fs::canonicalize(dir)?;
    for e in &snapshot.entries {
        check_action_path(&e.path)?;
        for b in e.file.iter().chain(e.index.iter()) {
            check_blob(b)?;
        }
    }
    check_unchanged_since_discard(git, dir, &root, snapshot).await?;
    let mut index_info = Vec::new();
    let mut unindex = Vec::new();
    for e in snapshot.entries.iter().filter(|e| e.restore_index) {
        match &e.index {
            Some(b) => {
                index_info.extend(format!("{:o} {}\t{}\0", b.mode, b.oid, e.path).into_bytes())
            }
            None => unindex.push(e.path.clone()),
        }
    }
    if !unindex.is_empty() {
        write(
            git,
            dir,
            &["rm", "--cached", "-q", "-r", "--ignore-unmatch"],
            &unindex,
        )
        .await?;
    }
    if !index_info.is_empty() {
        with_lock_retry(|| {
            git.output_with_stdin(
                dir,
                &["update-index", "-z", "--index-info"],
                Some(index_info.clone()),
            )
        })
        .await?;
    }
    for e in &snapshot.entries {
        match &e.file {
            Some(b) => {
                let data = read_blob(git, dir, &b.oid).await?;
                write_file(&root, &e.path, &data, b.mode)?;
            }
            None => remove_file(&root, &e.path)?,
        }
    }
    Ok(())
}

/// Each path still is as [`discard`] left it: no file for a deleted
/// untracked one; for a staged discard, nothing in `git status` (index and
/// file match HEAD); otherwise no working-tree change against the index.
async fn check_unchanged_since_discard(
    git: &Git,
    dir: &Path,
    root: &Path,
    snapshot: &GitDiscardSnapshot,
) -> Result<(), SessionError> {
    let paths: Vec<&str> = snapshot.entries.iter().map(|e| e.path.as_str()).collect();
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "--no-optional-locks",
        "--literal-pathspecs",
        "status",
        "--porcelain=v1",
        "-z",
        "--no-renames",
        "--untracked-files=all",
        "--",
    ];
    args.extend(&paths);
    let out = git.run(dir, &args).await?;
    // "XY path\0"
    let status: HashMap<&str, &[u8]> = out
        .split('\0')
        .filter(|r| r.len() > 3)
        .map(|r| (&r[3..], &r.as_bytes()[..2]))
        .collect();
    for e in &snapshot.entries {
        let changed = match status.get(e.path.as_str()) {
            _ if e.untracked => std::fs::symlink_metadata(inside(root, &e.path)?).is_ok(),
            None => false,
            Some(_) if e.restore_index => true,
            Some(xy) => xy[1] != b' ',
        };
        if changed {
            return Err(SessionError::InvalidArgument(format!(
                "{} changed since the discard; undoing it would overwrite that",
                e.path
            )));
        }
    }
    Ok(())
}

/// `git ls-files -s` of `paths`: path → entry (the last stage of a
/// conflicted path).
async fn index_entries(
    git: &Git,
    dir: &Path,
    paths: &[String],
) -> Result<HashMap<String, IndexEntry>, SessionError> {
    let mut args = vec!["--literal-pathspecs", "ls-files", "-s", "-z", "--"];
    args.extend(paths.iter().map(String::as_str));
    let out = git.run(dir, &args).await?;
    let mut map = HashMap::new();
    for rec in out.split('\0').filter(|r| !r.is_empty()) {
        // mode SP oid SP stage TAB path
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let mut f = meta.split(' ');
        let (Some(mode), Some(oid), Some(stage)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (Ok(mode), Ok(stage)) = (u32::from_str_radix(mode, 8), stage.parse()) else {
            continue;
        };
        map.insert(
            path.to_owned(),
            IndexEntry {
                blob: GitBlobRef {
                    oid: oid.to_owned(),
                    mode,
                },
                stage,
            },
        );
    }
    Ok(map)
}

/// Which of `paths` HEAD has (as files, symlinks or submodules).
async fn head_paths(
    git: &Git,
    dir: &Path,
    paths: &[String],
) -> Result<HashSet<String>, SessionError> {
    let mut args = vec!["ls-tree", "-z", "--full-tree", "--name-only", "HEAD", "--"];
    args.extend(paths.iter().map(String::as_str));
    let out = git.run(dir, &args).await?;
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    Ok(out
        .split('\0')
        .filter(|p| wanted.contains(p))
        .map(str::to_owned)
        .collect())
}

/// Write each existing working-tree file (or symlink) of `paths` to the
/// object database. Directories (a nested repository) are refused.
async fn snapshot_files(
    git: &Git,
    dir: &Path,
    root: &Path,
    paths: &[String],
) -> Result<HashMap<String, GitBlobRef>, SessionError> {
    let mut regular = Vec::new();
    let mut out = HashMap::new();
    for p in paths {
        let full = inside(root, p)?;
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.is_dir() {
            return Err(SessionError::InvalidArgument(format!(
                "{p} is a directory (a nested repository?)"
            )));
        }
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full)?;
            let oid = hash_blob(git, dir, target.as_os_str().as_encoded_bytes().to_vec()).await?;
            out.insert(
                p.clone(),
                GitBlobRef {
                    oid,
                    mode: MODE_SYMLINK,
                },
            );
        } else {
            let mode = if meta.permissions().mode() & 0o111 != 0 {
                MODE_EXEC
            } else {
                MODE_FILE
            };
            regular.push((p.clone(), mode));
        }
    }
    if !regular.is_empty() {
        let stdin: String = regular.iter().map(|(p, _)| format!("{p}\n")).collect();
        let res = git
            .output_with_stdin(
                dir,
                &["hash-object", "-w", "--no-filters", "--stdin-paths"],
                Some(stdin.into_bytes()),
            )
            .await?;
        let oids: Vec<&str> = res.stdout.lines().collect();
        if !res.success || oids.len() != regular.len() {
            return Err(failure("git hash-object", &res));
        }
        for ((p, mode), oid) in regular.into_iter().zip(oids) {
            out.insert(
                p,
                GitBlobRef {
                    oid: oid.to_owned(),
                    mode,
                },
            );
        }
    }
    Ok(out)
}

async fn hash_blob(git: &Git, dir: &Path, data: Vec<u8>) -> Result<String, SessionError> {
    let out = git
        .output_with_stdin(dir, &["hash-object", "-w", "--stdin"], Some(data))
        .await?;
    if !out.success {
        return Err(failure("git hash-object", &out));
    }
    Ok(out.stdout.trim().to_owned())
}

async fn read_blob(git: &Git, dir: &Path, oid: &str) -> Result<Vec<u8>, SessionError> {
    let out = git
        .output_limited(dir, &["cat-file", "blob", oid], usize::MAX)
        .await?;
    if out.code != Some(0) {
        return Err(SessionError::Git(if out.stderr.is_empty() {
            format!("blob {oid} is gone")
        } else {
            out.stderr
        }));
    }
    Ok(out.stdout)
}

fn check_blob(b: &GitBlobRef) -> Result<(), SessionError> {
    let hex = (4..=64).contains(&b.oid.len()) && b.oid.bytes().all(|c| c.is_ascii_hexdigit());
    if !hex || ![MODE_FILE, MODE_EXEC, MODE_SYMLINK].contains(&b.mode) {
        return Err(SessionError::InvalidArgument(format!(
            "bad snapshot entry {} {:o}",
            b.oid, b.mode
        )));
    }
    Ok(())
}

/// `root/path` with its existing ancestors resolved: refused if a symlinked
/// directory leads outside `root` (canonical).
fn inside(root: &Path, path: &str) -> Result<PathBuf, SessionError> {
    let full = root.join(path);
    let mut existing = full.parent();
    while let Some(d) = existing {
        if let Ok(c) = std::fs::canonicalize(d) {
            if !c.starts_with(root) {
                return Err(SessionError::InvalidArgument(format!(
                    "{path} is outside the worktree"
                )));
            }
            return Ok(full);
        }
        existing = d.parent();
    }
    Err(SessionError::InvalidArgument(format!(
        "{path} is outside the worktree"
    )))
}

/// Delete a file or symlink (not a directory), then any directories it
/// leaves empty up to the root.
fn remove_file(root: &Path, path: &str) -> Result<(), SessionError> {
    let full = inside(root, path)?;
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_dir() => {
            return Err(SessionError::InvalidArgument(format!(
                "{path} is a directory"
            )));
        }
        Ok(_) => std::fs::remove_file(&full)?,
        Err(_) => return Ok(()),
    }
    let mut d = full.parent();
    while let Some(dir) = d {
        if dir == root || std::fs::remove_dir(dir).is_err() {
            break;
        }
        d = dir.parent();
    }
    Ok(())
}

fn write_file(root: &Path, path: &str, data: &[u8], mode: u32) -> Result<(), SessionError> {
    let full = inside(root, path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_dir() => {
            return Err(SessionError::InvalidArgument(format!(
                "{path} is a directory now"
            )));
        }
        Ok(_) => std::fs::remove_file(&full)?,
        Err(_) => {}
    }
    if mode == MODE_SYMLINK {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(data), &full)?;
    } else {
        std::fs::write(&full, data)?;
        let perm = if mode == MODE_EXEC { 0o755 } else { 0o644 };
        std::fs::set_permissions(&full, std::fs::Permissions::from_mode(perm))?;
    }
    Ok(())
}

// ----- commit / conflicts / undo ------------------------------------------------

async fn commit(
    git: &Git,
    dir: &Path,
    message: &str,
    amend: bool,
    all: bool,
) -> Result<(), SessionError> {
    let message = message.trim();
    if message.is_empty() && !amend {
        return Err(SessionError::InvalidArgument(
            "the commit message is empty".into(),
        ));
    }
    if all {
        with_lock_retry(|| git.output(dir, &["add", "-A"])).await?;
    }
    let mut args = vec!["commit", "--quiet"];
    if amend {
        args.push("--amend");
    }
    let stdin = if message.is_empty() {
        args.push("--no-edit");
        None
    } else {
        args.extend(["-F", "-"]);
        Some(message.as_bytes().to_vec())
    };
    with_lock_retry(|| git.output_with_stdin(dir, &args, stdin.clone())).await?;
    Ok(())
}

async fn accept_conflict(
    git: &Git,
    dir: &Path,
    paths: &[String],
    side: ConflictSide,
) -> Result<(), SessionError> {
    // Unmerged stages per path: 1 base, 2 ours, 3 theirs.
    let mut args = vec!["--literal-pathspecs", "ls-files", "-u", "-z", "--"];
    args.extend(paths.iter().map(String::as_str));
    let out = git.run(dir, &args).await?;
    let mut stages: HashMap<&str, HashSet<u32>> = HashMap::new();
    for rec in out.split('\0').filter(|r| !r.is_empty()) {
        if let Some((meta, path)) = rec.split_once('\t') {
            if let Some(stage) = meta.split(' ').nth(2).and_then(|s| s.parse().ok()) {
                stages.entry(path).or_default().insert(stage);
            }
        }
    }
    let wanted = match side {
        ConflictSide::Ours => 2,
        ConflictSide::Theirs => 3,
    };
    let mut take = Vec::new();
    let mut remove = Vec::new();
    for p in paths {
        match stages.get(p.as_str()) {
            None => {
                return Err(SessionError::InvalidArgument(format!(
                    "{p} has no merge conflict"
                )));
            }
            Some(s) if s.contains(&wanted) => take.push(p.clone()),
            Some(_) => remove.push(p.clone()),
        }
    }
    if !take.is_empty() {
        let flag = match side {
            ConflictSide::Ours => "--ours",
            ConflictSide::Theirs => "--theirs",
        };
        write(git, dir, &["checkout", flag], &take).await?;
        write(git, dir, &["add"], &take).await?;
    }
    if !remove.is_empty() {
        // That side deleted the file: resolving to it removes it.
        write(git, dir, &["rm", "-q", "-f"], &remove).await?;
    }
    Ok(())
}

/// Move the branch back to HEAD's parent, keeping index and files. A
/// compare-and-swap on HEAD, so a commit an agent made meanwhile is never
/// the one undone.
async fn undo_commit(git: &Git, dir: &Path, head: &str) -> Result<(), SessionError> {
    let info = branch(git, dir).await?;
    if info.head.as_deref() != Some(head) {
        return Err(SessionError::InvalidArgument(
            "HEAD moved; refresh and try again".into(),
        ));
    }
    if !info.can_undo_commit {
        return Err(SessionError::InvalidArgument(
            "the last commit is already pushed (or has no parent)".into(),
        ));
    }
    let parent = format!("{head}^");
    let args = [
        "update-ref",
        "-m",
        "civet: undo last commit",
        "HEAD",
        &parent,
        head,
    ];
    with_lock_retry(|| git.output(dir, &args)).await?;
    Ok(())
}

// ----- push / pull / fetch ------------------------------------------------------

async fn push(git: &Git, dir: &Path) -> Result<(), SessionError> {
    let branch = current_branch(git, dir, "push").await?;
    let configured = git
        .output(
            dir,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )
        .await?;
    if configured.success && !configured.stdout.trim().is_empty() {
        network(git, dir, &["push"]).await?;
        return Ok(());
    }
    let remote = default_remote(git, dir).await?;
    let spec = format!("refs/heads/{branch}:refs/heads/{branch}");
    network(git, dir, &["push", "-u", &remote, &spec]).await?;
    Ok(())
}

async fn pull(git: &Git, dir: &Path) -> Result<(), SessionError> {
    let branch = current_branch(git, dir, "pull").await?;
    let upstream = git
        .output(dir, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .await?;
    if !upstream.success {
        return Err(SessionError::Git(format!(
            "{branch} has no upstream branch to pull from (push it first)"
        )));
    }
    let mut args = vec!["-c", "core.editor=true", "pull", "--no-edit"];
    // Why: without a configured strategy, git refuses to pull divergent
    // branches; a merge is what it did before it started asking.
    let (rebase, ff) = tokio::join!(
        git.output(dir, &["config", "--get", "pull.rebase"]),
        git.output(dir, &["config", "--get", "pull.ff"]),
    );
    if !rebase?.success && !ff?.success {
        args.push("--no-rebase");
    }
    network(git, dir, &args).await?;
    Ok(())
}

async fn current_branch(git: &Git, dir: &Path, what: &str) -> Result<String, SessionError> {
    let out = git
        .output(dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;
    let b = out.stdout.trim();
    if !out.success || b.is_empty() {
        return Err(SessionError::Git(format!(
            "HEAD is detached; check out a branch to {what}"
        )));
    }
    Ok(b.to_owned())
}

/// `origin` if there is one, else the only remote.
async fn default_remote(git: &Git, dir: &Path) -> Result<String, SessionError> {
    let out = git.run(dir, &["remote"]).await?;
    let remotes: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    if remotes.contains(&"origin") {
        return Ok("origin".into());
    }
    match remotes.as_slice() {
        [only] => Ok((*only).to_owned()),
        [] => Err(SessionError::Git("this repository has no remote".into())),
        _ => Err(SessionError::Git(format!(
            "no upstream and no origin remote; pick one of {} in a terminal",
            remotes.join(", ")
        ))),
    }
}

/// A command that talks to a remote: bounded by [`NETWORK_TIMEOUT`].
async fn network(git: &Git, dir: &Path, args: &[&str]) -> Result<String, SessionError> {
    match tokio::time::timeout(NETWORK_TIMEOUT, with_lock_retry(|| git.output(dir, args))).await {
        Ok(r) => r,
        Err(_) => Err(SessionError::Git(format!(
            "git {} timed out after {} s",
            args.iter()
                .find(|a| !a.starts_with('-') && !a.contains('='))
                .unwrap_or(&""),
            NETWORK_TIMEOUT.as_secs()
        ))),
    }
}

// ----- stash ----------------------------------------------------------------------

async fn stash_all(git: &Git, dir: &Path) -> Result<(), SessionError> {
    let out = with_lock_retry(|| {
        git.output(
            dir,
            &["stash", "push", "--include-untracked", "-m", STASH_MESSAGE],
        )
    })
    .await?;
    if out.contains("No local changes to save") {
        return Err(SessionError::Git("no local changes to stash".into()));
    }
    Ok(())
}

/// Apply the newest stash entry of this branch, then drop it by its id (the
/// shared stash list may shift meanwhile). If applying conflicts, the entry
/// is kept.
async fn stash_pop(git: &Git, dir: &Path) -> Result<(), SessionError> {
    let branch = git
        .output(dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;
    let branch = branch.success.then(|| branch.stdout.trim().to_owned());
    let Some((oid, _)) = branch_stashes(git, dir, branch.as_deref())
        .await?
        .into_iter()
        .next()
    else {
        return Err(SessionError::Git("no stash entries for this branch".into()));
    };
    let apply = ["stash", "apply", oid.as_str()];
    if let Err(e) = with_lock_retry(|| git.output(dir, &apply)).await {
        return Err(SessionError::Git(format!(
            "{}\n(the stash entry is kept)",
            e.to_string().trim_start_matches("git: ")
        )));
    }
    let list = git.run(dir, &["stash", "list", "--format=%gd %H"]).await?;
    if let Some(r) = list
        .lines()
        .find_map(|l| l.split_once(' ').filter(|(_, h)| *h == oid).map(|(r, _)| r))
    {
        git.run(dir, &["stash", "drop", "-q", r]).await?;
    }
    Ok(())
}

/// Stash entries made on `branch` (None: detached), newest first:
/// (commit id, reflog subject).
async fn branch_stashes(
    git: &Git,
    dir: &Path,
    branch: Option<&str>,
) -> Result<Vec<(String, String)>, SessionError> {
    let out = git
        .output(
            dir,
            &["--no-optional-locks", "stash", "list", "--format=%H %gs"],
        )
        .await?;
    if !out.success {
        return Ok(Vec::new());
    }
    let name = branch.unwrap_or("(no branch)");
    let (on, wip) = (format!("On {name}: "), format!("WIP on {name}: "));
    Ok(out
        .stdout
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, s)| s.starts_with(&on) || s.starts_with(&wip))
        .map(|(h, s)| (h.to_owned(), s.to_owned()))
        .collect())
}

// ----- helpers ----------------------------------------------------------------------

/// Validated, de-duplicated paths (order kept): at least one, none through
/// a symlinked directory out of the worktree at `dir`.
fn action_paths(dir: &Path, paths: &[String]) -> Result<Vec<String>, SessionError> {
    if paths.is_empty() {
        return Err(SessionError::InvalidArgument("no paths given".into()));
    }
    let root = std::fs::canonicalize(dir)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        check_action_path(p)?;
        inside(&root, p)?;
        if seen.insert(p.as_str()) {
            out.push(p.clone());
        }
    }
    Ok(out)
}

/// [`check_relative_path`], and never inside `.git`.
fn check_action_path(path: &str) -> Result<(), SessionError> {
    check_relative_path(path)?;
    if Path::new(path)
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err(SessionError::InvalidArgument(format!(
            "{path:?} is inside .git"
        )));
    }
    Ok(())
}

async fn has_head(git: &Git, dir: &Path) -> Result<bool, SessionError> {
    git.succeeds(
        dir,
        &[
            "--no-optional-locks",
            "rev-parse",
            "-q",
            "--verify",
            "HEAD^{commit}",
        ],
    )
    .await
}

/// `git --literal-pathspecs <args> --pathspec-from-file=- --pathspec-file-nul`
/// with `paths` on stdin (no argv limit, no pathspec magic).
async fn write(git: &Git, dir: &Path, args: &[&str], paths: &[String]) -> Result<(), SessionError> {
    let mut a = vec!["--literal-pathspecs"];
    a.extend_from_slice(args);
    a.extend(["--pathspec-from-file=-", "--pathspec-file-nul"]);
    let stdin: Vec<u8> = paths
        .iter()
        .flat_map(|p| p.bytes().chain(std::iter::once(0)))
        .collect();
    with_lock_retry(|| git.output_with_stdin(dir, &a, Some(stdin.clone()))).await?;
    Ok(())
}

/// Run a git command, retrying while another git holds `index.lock`.
/// Returns stdout; the error carries git's stderr (or stdout, where git
/// reports e.g. "nothing to commit" or merge conflicts).
async fn with_lock_retry<F, Fut>(mut f: F) -> Result<String, SessionError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<GitOutput, SessionError>>,
{
    let mut tries = 0;
    loop {
        let out = f().await?;
        if out.success {
            return Ok(out.stdout);
        }
        tries += 1;
        if tries < LOCK_TRIES && out.stderr.contains("index.lock") {
            tokio::time::sleep(LOCK_RETRY_DELAY).await;
            continue;
        }
        return Err(failure("git", &out));
    }
}

fn failure(what: &str, out: &GitOutput) -> SessionError {
    let msg = [out.stdout.trim(), out.stderr.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    SessionError::Git(if msg.is_empty() {
        format!("{what} failed")
    } else {
        msg
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_paths_are_checked() {
        for bad in [".git/config", "sub/.GIT/x", "../x", "/abs", ""] {
            assert!(check_action_path(bad).is_err(), "{bad}");
        }
        assert!(check_action_path("a/.gitignore").is_ok());
        let dir = tempfile::tempdir().unwrap();
        assert!(action_paths(dir.path(), &[]).is_err());
        assert_eq!(
            action_paths(dir.path(), &["a".into(), "b".into(), "a".into()]).unwrap(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn blob_refs_are_checked() {
        let b = |oid: &str, mode| GitBlobRef {
            oid: oid.into(),
            mode,
        };
        assert!(check_blob(&b("e69de29b", MODE_FILE)).is_ok());
        assert!(check_blob(&b("--help", MODE_FILE)).is_err());
        assert!(check_blob(&b("e69de29b", 0o40000)).is_err());
    }
}
