//! Changing a worktree's files (the desktop's Files tab): create, rename /
//! move, copy, write, trash and restore ([`FileOp`]).
//!
//! Path rules are those of [`crate::files`], plus: no path may name `.git`
//! or anything in it (checked on the canonical path too, so a symlink to
//! `.git` does not get around it), the worktree root itself cannot be moved
//! or trashed, and nothing is ever overwritten (`AlreadyExists`). Parents
//! are resolved canonically; the last component is not followed, so moving
//! or trashing a symlink acts on the link.
//!
//! Trash lives in `<state>/trash/<id>/` (`item` plus `meta.json` with the
//! worktree and path it came from), outside every worktree. Entries older
//! than [`TRASH_MAX_AGE`], and all but the newest [`TRASH_MAX_ENTRIES`], are
//! purged whenever something is trashed.

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use civet_proto::{FileOp, FileOpResult, MAX_UPLOAD_BYTES};
use serde::{Deserialize, Serialize};

use crate::SessionError;
use crate::files::{blocking, canonical_root, resolve};

/// Trash entries older than this are purged.
pub(crate) const TRASH_MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);
/// At most this many trash entries are kept (newest first).
pub(crate) const TRASH_MAX_ENTRIES: usize = 200;

const TRASH_ITEM: &str = "item";
const TRASH_META: &str = "meta.json";

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Where a trashed entry came from (`meta.json`).
#[derive(Debug, Serialize, Deserialize)]
struct TrashMeta {
    /// Canonical worktree root.
    root: String,
    /// Path relative to it.
    path: String,
    trashed_at_ms: u64,
}

/// Apply `op` in the worktree at `root`, trashing into `trash`.
pub(crate) async fn apply(
    root: &Path,
    trash: PathBuf,
    op: FileOp,
) -> Result<FileOpResult, SessionError> {
    let root = canonical_root(root)?;
    blocking(move || apply_sync(&root, &trash, op)).await
}

fn done(path: &str) -> FileOpResult {
    FileOpResult {
        path: normalize(path),
        trash_id: None,
    }
}

fn apply_sync(root: &Path, trash: &Path, op: FileOp) -> Result<FileOpResult, SessionError> {
    match op {
        FileOp::CreateFile { path } => {
            let target = fresh(root, &path)?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| io(&path, e))?;
            Ok(done(&path))
        }
        FileOp::CreateDir { path } => {
            let target = fresh(root, &path)?;
            std::fs::create_dir(&target).map_err(|e| io(&path, e))?;
            Ok(done(&path))
        }
        FileOp::Write { path, data } => {
            if data.len() > MAX_UPLOAD_BYTES {
                return Err(SessionError::InvalidArgument(format!(
                    "{path:?} is {} bytes; the limit is {MAX_UPLOAD_BYTES}",
                    data.len()
                )));
            }
            let target = fresh(root, &path)?;
            write_new(&target, &data).map_err(|e| io(&path, e))?;
            Ok(done(&path))
        }
        FileOp::Move { from, to } => {
            let src = existing(root, &from)?;
            refuse_repo_dir(&src, &from)?;
            let dst = target(root, &to)?;
            if let Ok(meta) = std::fs::symlink_metadata(&dst) {
                // A case-only rename on a case-insensitive file system: the
                // "existing" target is the source itself.
                let src_meta = std::fs::symlink_metadata(&src)?;
                let same = (meta.dev(), meta.ino()) == (src_meta.dev(), src_meta.ino());
                if !same || src == dst || !case_variants(&src, &dst) {
                    return Err(exists(&to));
                }
                std::fs::rename(&src, &dst).map_err(|e| io(&to, e))?;
                return Ok(done(&to));
            }
            refuse_into_itself(&src, &dst, &from)?;
            move_entry(&src, &dst).map_err(|e| io(&to, e.into_io()))?;
            Ok(done(&to))
        }
        FileOp::Copy { from, to } => {
            let src = existing(root, &from)?;
            let dst = fresh(root, &to)?;
            refuse_into_itself(&src, &dst, &from)?;
            copy_new(&src, &dst, Keep::Files).map_err(|e| io(&to, e))?;
            Ok(done(&to))
        }
        FileOp::Trash { path } => {
            let src = existing(root, &path)?;
            refuse_repo_dir(&src, &path)?;
            let id = trash_entry(root, trash, &src, &path)?;
            purge_trash(trash, civet_proto::now_ms());
            Ok(FileOpResult {
                path: normalize(&path),
                trash_id: Some(id),
            })
        }
        FileOp::Restore { trash_id } => restore(root, trash, &trash_id),
    }
}

/// `rel` without `.` components or doubled slashes.
fn normalize(rel: &str) -> String {
    let parts: Vec<_> = Path::new(rel)
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy()),
            _ => None,
        })
        .collect();
    parts.join("/")
}

fn is_git(name: &OsStr) -> bool {
    name.as_encoded_bytes().eq_ignore_ascii_case(b".git")
}

/// Same directory, names equal but for (ASCII or Unicode) case.
fn case_variants(a: &Path, b: &Path) -> bool {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    a.parent() == b.parent() && name(a) == name(b)
}

fn io(path: &str, e: std::io::Error) -> SessionError {
    match e.kind() {
        std::io::ErrorKind::AlreadyExists => exists(path),
        std::io::ErrorKind::NotFound => SessionError::NotFound(format!("{path:?}")),
        _ => SessionError::Io(std::io::Error::new(e.kind(), format!("{path:?}: {e}"))),
    }
}

fn exists(path: &str) -> SessionError {
    SessionError::AlreadyExists(format!("{path:?}"))
}

/// The parent directory of `rel` (canonical, inside `root`, not in `.git`)
/// joined with its last component, which is not followed. InvalidArgument
/// for the root itself, `..`, absolute paths and anything naming `.git`.
fn target(root: &Path, rel: &str) -> Result<PathBuf, SessionError> {
    let invalid = |why: &str| SessionError::InvalidArgument(format!("{rel:?} {why}"));
    if rel.contains('\0') {
        return Err(invalid("is outside the worktree"));
    }
    let mut names = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(name) if is_git(name) => {
                return Err(invalid("is in .git, which is off limits"));
            }
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid("is outside the worktree"));
            }
        }
    }
    let Some(name) = names.pop() else {
        return Err(invalid("is the worktree root"));
    };
    let parent: PathBuf = names.iter().collect();
    let dir = resolve(root, &parent.to_string_lossy())?;
    if !dir.is_dir() {
        return Err(SessionError::InvalidArgument(format!(
            "{:?} is not a directory",
            parent.to_string_lossy()
        )));
    }
    // A symlink on the way may lead into .git.
    let inside = dir.strip_prefix(root).unwrap_or(Path::new(""));
    if inside.components().any(|c| is_git(c.as_os_str())) {
        return Err(invalid("is in .git, which is off limits"));
    }
    Ok(dir.join(name))
}

/// [`target`] that must exist (`lstat`).
fn existing(root: &Path, rel: &str) -> Result<PathBuf, SessionError> {
    let path = target(root, rel)?;
    std::fs::symlink_metadata(&path).map_err(|e| io(rel, e))?;
    Ok(path)
}

/// [`target`] that must not exist yet.
fn fresh(root: &Path, rel: &str) -> Result<PathBuf, SessionError> {
    let path = target(root, rel)?;
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(exists(rel));
    }
    Ok(path)
}

/// A directory into its own subtree would never end (copy) or fail
/// obscurely (rename).
fn refuse_into_itself(src: &Path, dst: &Path, rel: &str) -> Result<(), SessionError> {
    if dst.starts_with(src) {
        return Err(SessionError::InvalidArgument(format!(
            "{rel:?} cannot go inside itself"
        )));
    }
    Ok(())
}

/// A nested repository or submodule checkout carries its own `.git`, which
/// is never moved or deleted.
fn refuse_repo_dir(src: &Path, rel: &str) -> Result<(), SessionError> {
    let is_dir = std::fs::symlink_metadata(src).is_ok_and(|m| m.is_dir());
    if is_dir && std::fs::symlink_metadata(src.join(".git")).is_ok() {
        return Err(SessionError::InvalidArgument(format!(
            "{rel:?} is a git repository (it has a .git), which is off limits"
        )));
    }
    Ok(())
}

/// A new file with `data`; removed again if writing fails.
fn write_new(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let r = f.write_all(data).and_then(|()| f.sync_all());
    if r.is_err() {
        let _ = std::fs::remove_file(path);
    }
    r
}

/// A failed [`move_entry`].
#[derive(Debug)]
enum MoveError {
    /// Nothing moved; `src` is as it was.
    Failed(std::io::Error),
    /// Across file systems: `dst` is a complete copy, but removing `src`
    /// failed part way. `dst` must be kept (it may be the only whole copy).
    Copied(std::io::Error),
}

impl MoveError {
    fn into_io(self) -> std::io::Error {
        match self {
            MoveError::Failed(e) => e,
            MoveError::Copied(e) => std::io::Error::new(
                e.kind(),
                format!("copied, but the original could not be removed completely: {e}"),
            ),
        }
    }
}

/// Rename without replacing; across file systems, copy everything
/// (`.git` included) then delete. Special files make such a move fail
/// before anything is deleted.
fn move_entry(src: &Path, dst: &Path) -> Result<(), MoveError> {
    match crate::sys::rename_noreplace(src, dst) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_new(src, dst, Keep::All).map_err(MoveError::Failed)?;
            remove_entry(src).map_err(MoveError::Copied)
        }
        r => r.map_err(MoveError::Failed),
    }
}

fn remove_entry(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// What [`copy_tree`] copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// The user's Copy: `.git` entries left out, special files skipped.
    Files,
    /// A move across file systems: everything; special files are an error.
    All,
}

/// [`copy_tree`] to a path that does not exist; whatever was copied is
/// removed again if it fails.
fn copy_new(src: &Path, dst: &Path, keep: Keep) -> std::io::Result<()> {
    let r = copy_tree(src, dst, keep);
    if let Err(e) = &r
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        let _ = remove_entry(dst);
    }
    r
}

/// Copy a file, symlink (as a link) or directory tree, replacing nothing.
/// With [`Keep::Files`], `.git` entries are left out and fifos, sockets and
/// devices skipped; with [`Keep::All`] those fail the copy.
fn copy_tree(src: &Path, dst: &Path, keep: Keep) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if ft.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            if keep == Keep::Files && is_git(&name) {
                continue;
            }
            copy_tree(&entry.path(), &dst.join(&name), keep)?;
        }
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(meta.mode() & 0o7777))
    } else if ft.is_file() {
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(meta.mode() & 0o777)
            .open(dst)?;
        std::io::copy(&mut from, &mut to)?;
        Ok(())
    } else if keep == Keep::All {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "{} is a fifo, socket or device, which can't be moved to another file system",
                src.display()
            ),
        ))
    } else {
        Ok(())
    }
}

/// `<ms>-<pid>-<seq>`: sorts by age, and the age is readable without
/// opening `meta.json`.
fn trash_id(now_ms: u64) -> String {
    format!(
        "{now_ms}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn valid_trash_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

fn trash_entry(root: &Path, trash: &Path, src: &Path, rel: &str) -> Result<String, SessionError> {
    std::fs::create_dir_all(trash)?;
    std::fs::set_permissions(trash, std::fs::Permissions::from_mode(0o700))?;
    let now = civet_proto::now_ms();
    let id = trash_id(now);
    let dir = trash.join(&id);
    std::fs::create_dir(&dir)?;
    let meta = TrashMeta {
        root: root.to_string_lossy().into_owned(),
        path: normalize(rel),
        trashed_at_ms: now,
    };
    let r = std::fs::write(
        dir.join(TRASH_META),
        serde_json::to_vec(&meta).map_err(std::io::Error::other)?,
    )
    .map_err(MoveError::Failed)
    .and_then(|()| move_entry(src, &dir.join(TRASH_ITEM)));
    match r {
        Ok(()) => Ok(id),
        // The trash holds the only complete copy: keep it (restorable).
        Err(e @ MoveError::Copied(_)) => Err(io(rel, e.into_io())),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(io(rel, e.into_io()))
        }
    }
}

fn restore(root: &Path, trash: &Path, id: &str) -> Result<FileOpResult, SessionError> {
    let gone = || SessionError::NotFound(format!("trash entry {id:?}"));
    if !valid_trash_id(id) {
        return Err(gone());
    }
    let dir = trash.join(id);
    let meta: TrashMeta = std::fs::read(dir.join(TRASH_META))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(gone)?;
    // Only into the worktree it came from.
    if Path::new(&meta.root) != root {
        return Err(gone());
    }
    let item = dir.join(TRASH_ITEM);
    if std::fs::symlink_metadata(&item).is_err() {
        return Err(gone());
    }
    let dst = fresh(root, &meta.path)?;
    match move_entry(&item, &dst) {
        // Back in the worktree; what is left of the trash copy is purged.
        Ok(()) | Err(MoveError::Copied(_)) => {}
        Err(e) => return Err(io(&meta.path, e.into_io())),
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(done(&meta.path))
}

/// Drop trash entries past [`TRASH_MAX_AGE`] or beyond the newest
/// [`TRASH_MAX_ENTRIES`]. Best effort: failures are logged, not returned.
pub(crate) fn purge_trash(trash: &Path, now_ms: u64) {
    let Ok(rd) = std::fs::read_dir(trash) else {
        return;
    };
    let mut entries: Vec<(u64, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let ms = name.split('-').next()?.parse().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    entries.sort_unstable_by(|a, b| b.cmp(a));
    let max_age = TRASH_MAX_AGE.as_millis() as u64;
    for (i, (ms, path)) in entries.iter().enumerate() {
        if i >= TRASH_MAX_ENTRIES || now_ms.saturating_sub(*ms) > max_age {
            if let Err(e) = std::fs::remove_dir_all(path) {
                tracing::debug!(path = %path.display(), error = %e, "trash purge failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("./a//b/"), "a/b");
        assert_eq!(normalize("a"), "a");
    }

    #[test]
    fn git_names() {
        assert!(is_git(OsStr::new(".git")));
        assert!(is_git(OsStr::new(".GIT")));
        assert!(!is_git(OsStr::new(".github")));
        assert!(!is_git(OsStr::new(".gitignore")));
    }

    #[test]
    fn trash_ids() {
        let id = trash_id(1234);
        assert!(id.starts_with("1234-"));
        assert!(valid_trash_id(&id));
        assert!(!valid_trash_id("../x"));
        assert!(!valid_trash_id(""));
    }

    #[test]
    fn moves_copy_everything_or_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("vendor");
        std::fs::create_dir_all(src.join("lib/.git")).unwrap();
        std::fs::write(src.join("lib/.git/HEAD"), "ref").unwrap();
        std::fs::write(src.join("lib/a.rs"), "a").unwrap();
        // The user's Copy leaves .git out; a move's copy keeps it.
        copy_new(&src, &tmp.path().join("copy"), Keep::Files).unwrap();
        assert!(!tmp.path().join("copy/lib/.git").exists());
        copy_new(&src, &tmp.path().join("moved"), Keep::All).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("moved/lib/.git/HEAD")).unwrap(),
            "ref"
        );
        // A special file fails a move's copy, and nothing of it is left.
        let fifo =
            std::ffi::CString::new(src.join("lib/pipe").into_os_string().into_encoded_bytes())
                .unwrap();
        // SAFETY: a valid NUL-terminated path, alive for the call.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
        assert_eq!(rc, 0);
        let err = copy_new(&src, &tmp.path().join("again"), Keep::All).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(!tmp.path().join("again").exists());
        copy_new(&src, &tmp.path().join("files"), Keep::Files).unwrap();
        assert!(!tmp.path().join("files/lib/pipe").exists());
    }

    #[test]
    fn purges_old_and_excess_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let day = 24 * 3600 * 1000;
        let now = 10 * day;
        let mut kept = Vec::new();
        for i in 0..(TRASH_MAX_ENTRIES as u64 + 5) {
            let id = format!("{}-1-{i}", now - i);
            std::fs::create_dir(tmp.path().join(&id)).unwrap();
            kept.push(id);
        }
        let old = format!("{}-1-0", now - 4 * day);
        std::fs::create_dir(tmp.path().join(&old)).unwrap();
        std::fs::create_dir(tmp.path().join("stray")).unwrap();
        purge_trash(tmp.path(), now);
        let left: std::collections::HashSet<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(!left.contains(&old));
        // The newest are kept, the rest dropped; unknown names left alone.
        assert_eq!(left.len(), TRASH_MAX_ENTRIES + 1);
        assert!(left.contains(&kept[0]) && !left.contains(kept.last().unwrap()));
        assert!(left.contains("stray"));
    }
}
