//! Read-only browsing of a worktree's files (the desktop's Files tab).
//!
//! Every path a client sends is relative to the worktree root and must stay
//! inside it: [`resolve`] rejects absolute paths and `..` components, then
//! canonicalizes (resolving symlinks) and checks the result is still under
//! the root. [`read_file`] checks again on the opened file, so a path swapped
//! for a symlink after `resolve` cannot make it read outside the worktree.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use civet_proto::{
    DirListing, FileEntry, FileKind, FileList, FilePreview, MAX_ALL_FILES, MAX_DIR_ENTRIES,
    MAX_FILE_PREVIEW_BYTES,
};

use crate::SessionError;
use crate::git::Git;

/// Binary detection looks for a NUL byte in this many leading bytes.
const BINARY_SNIFF_BYTES: usize = 8192;

/// `rel` (relative to `root`, which must be canonical) as a canonical path
/// inside `root`. InvalidArgument for absolute paths, `..` components and
/// anything whose canonical path (symlinks resolved) leaves `root`;
/// NotFound if it does not exist.
pub(crate) fn resolve(root: &Path, rel: &str) -> Result<PathBuf, SessionError> {
    let outside = || SessionError::InvalidArgument(format!("{rel:?} is outside the worktree"));
    if rel.contains('\0') {
        return Err(outside());
    }
    let mut joined = root.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(name) => joined.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(outside());
            }
        }
    }
    let canon = std::fs::canonicalize(&joined).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SessionError::NotFound(format!("{rel:?}")),
        _ => SessionError::Io(e),
    })?;
    if canon.starts_with(root) {
        Ok(canon)
    } else {
        Err(outside())
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, SessionError> {
    std::fs::canonicalize(root)
        .map_err(|_| SessionError::NotFound(format!("worktree directory {}", root.display())))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SessionError> + Send + 'static,
) -> Result<T, SessionError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?
}

/// One directory level: directories first, then by name (case-insensitive),
/// at most [`MAX_DIR_ENTRIES`]. Gitignored entries (one `git check-ignore
/// --stdin` call; tracked files are never reported as ignored) are left
/// out unless `include_ignored`. `.git` is never listed.
pub(crate) async fn list_dir(
    git: &Git,
    root: &Path,
    dir: &str,
    include_ignored: bool,
) -> Result<DirListing, SessionError> {
    let root = canonical_root(root)?;
    let target = resolve(&root, dir)?;
    if !target.is_dir() {
        return Err(SessionError::InvalidArgument(format!(
            "{dir:?} is not a directory"
        )));
    }
    let dir_path = target.clone();
    let mut entries = blocking(move || read_entries(&dir_path)).await?;

    // Bare names, checked from the directory itself: that is also right
    // inside a submodule or nested repository, which has its own rules.
    let mut stdin = Vec::new();
    for e in &entries {
        stdin.extend_from_slice(e.name.as_bytes());
        stdin.push(0);
    }
    let ignored = ignored_paths(git, &target, stdin).await?;
    for e in &mut entries {
        e.ignored = ignored.contains(&e.name);
    }
    if !include_ignored {
        entries.retain(|e| !e.ignored);
    }
    entries.sort_by(|a, b| {
        (a.kind != FileKind::Dir)
            .cmp(&(b.kind != FileKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let truncated = entries.len() > MAX_DIR_ENTRIES;
    entries.truncate(MAX_DIR_ENTRIES);
    Ok(DirListing { entries, truncated })
}

fn read_entries(dir: &Path) -> Result<Vec<FileEntry>, SessionError> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        // Vanished since read_dir: skip it.
        let Ok(meta) = entry.metadata() else { continue };
        let kind = if meta.is_dir() {
            FileKind::Dir
        } else if meta.is_symlink() {
            FileKind::Symlink
        } else {
            FileKind::File
        };
        out.push(FileEntry {
            name,
            kind,
            size: if kind == FileKind::Dir { 0 } else { meta.len() },
            ignored: false,
        });
    }
    Ok(out)
}

/// Which of the NUL-separated names in `stdin` (relative to `dir`) git
/// ignores. A failed check is an error: listing ignored entries as if they
/// were not would break the "hidden unless asked for" contract.
async fn ignored_paths(
    git: &Git,
    dir: &Path,
    stdin: Vec<u8>,
) -> Result<HashSet<String>, SessionError> {
    if stdin.is_empty() {
        return Ok(HashSet::new());
    }
    let out = git
        .output_with_stdin(dir, &["check-ignore", "-z", "--stdin"], Some(stdin))
        .await?;
    // Exit 1 with nothing on stderr: nothing ignored.
    if !out.success && !out.stderr.is_empty() {
        return Err(SessionError::Git(out.stderr));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The first `max_bytes` (at most [`MAX_FILE_PREVIEW_BYTES`]) of a regular
/// file. Binary if a NUL byte is in the first 8 KiB (no text then).
pub(crate) async fn read_file(
    root: &Path,
    path: &str,
    max_bytes: u64,
) -> Result<FilePreview, SessionError> {
    let root = canonical_root(root)?;
    let path = path.to_owned();
    blocking(move || {
        let outside = || SessionError::InvalidArgument(format!("{path:?} is outside the worktree"));
        let resolved = resolve(&root, &path)?;
        // Open once and check the handle from then on: O_NOFOLLOW refuses a
        // final symlink (the resolved path has none), O_NONBLOCK keeps a
        // fifo from blocking the open.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&resolved)
            .map_err(|e| match e.raw_os_error() {
                Some(libc::ELOOP) => outside(),
                _ => SessionError::Io(e),
            })?;
        // A directory on the way swapped for a symlink since `resolve`.
        if !crate::sys::fd_path(&file)?.starts_with(&root) {
            return Err(outside());
        }
        let meta = file.metadata()?;
        // Not a fifo or device: reading one could block forever.
        if !meta.is_file() {
            return Err(SessionError::InvalidArgument(format!(
                "{path:?} is not a regular file"
            )));
        }
        let cap = max_bytes.min(MAX_FILE_PREVIEW_BYTES);
        let mut buf = Vec::new();
        // One byte past the cap tells whether there is more.
        file.take(cap + 1).read_to_end(&mut buf)?;
        let truncated = buf.len() as u64 > cap;
        buf.truncate(cap as usize);
        let binary = buf[..buf.len().min(BINARY_SNIFF_BYTES)].contains(&0);
        Ok(FilePreview {
            size: meta.len().max(buf.len() as u64),
            binary,
            truncated,
            text: if binary {
                String::new()
            } else {
                decode_text(&buf, truncated)
            },
        })
    })
    .await
}

/// UTF-8 with invalid sequences replaced; a multi-byte character cut in
/// half by truncation is dropped instead.
fn decode_text(buf: &[u8], truncated: bool) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_owned(),
        Err(e) if truncated && e.error_len().is_none() => {
            String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(buf).into_owned(),
    }
}

/// Tracked and untracked, not ignored files (`git ls-files -z -co
/// --exclude-standard`), sorted, at most [`MAX_ALL_FILES`].
pub(crate) async fn list_all(git: &Git, root: &Path) -> Result<FileList, SessionError> {
    let root = canonical_root(root)?;
    let out = git
        .run(&root, &["ls-files", "-z", "-co", "--exclude-standard"])
        .await?;
    let mut paths: Vec<String> = out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    // Unmerged files are listed once per stage.
    paths.sort_unstable();
    paths.dedup();
    let truncated = paths.len() > MAX_ALL_FILES;
    paths.truncate(MAX_ALL_FILES);
    Ok(FileList { paths, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stays_inside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("wt");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "a").unwrap();
        std::fs::write(base.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(base.join("secret"), root.join("out")).unwrap();
        std::os::unix::fs::symlink(&base, root.join("up")).unwrap();
        std::os::unix::fs::symlink("src/a.rs", root.join("in")).unwrap();

        assert_eq!(resolve(&root, "").unwrap(), root);
        assert_eq!(resolve(&root, ".").unwrap(), root);
        assert_eq!(resolve(&root, "src/a.rs").unwrap(), root.join("src/a.rs"));
        assert_eq!(
            resolve(&root, "./src//a.rs").unwrap(),
            root.join("src/a.rs")
        );
        assert_eq!(resolve(&root, "in").unwrap(), root.join("src/a.rs"));
        for bad in [
            "..",
            "../secret",
            "src/../../secret",
            "src/..",
            "/etc/passwd",
            &*base.join("secret").to_string_lossy(),
            "out",
            "up",
            "up/secret",
            "src\0",
        ] {
            assert!(
                matches!(resolve(&root, bad), Err(SessionError::InvalidArgument(_))),
                "{bad:?} must be rejected"
            );
        }
        assert!(matches!(
            resolve(&root, "missing"),
            Err(SessionError::NotFound(_))
        ));
    }

    #[test]
    fn open_files_know_their_real_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::fs::write(base.join("real/f"), "x").unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let f = std::fs::File::open(base.join("link/f")).unwrap();
        assert_eq!(crate::sys::fd_path(&f).unwrap(), base.join("real/f"));
    }

    #[test]
    fn text_decoding() {
        assert_eq!(decode_text("héllo".as_bytes(), false), "héllo");
        // "é" cut in half by the size cap: dropped, not replaced.
        assert_eq!(decode_text(&"hé".as_bytes()[..2], true), "h");
        assert_eq!(decode_text(b"a\xffb", false), "a\u{fffd}b");
    }
}
