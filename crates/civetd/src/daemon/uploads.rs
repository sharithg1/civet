//! `UploadFile`: files dropped on a pane in the desktop app (e.g. screenshots
//! for an agent) land here so a remote pane can reference them by path. Each
//! upload gets its own directory under `$TMPDIR/civet-drops-<uid>/` and keeps its
//! original (sanitized) file name, which agents show to the user.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use civet_proto::{ErrorCode, MAX_UPLOAD_BYTES, RpcError};
use tokio::io::AsyncWriteExt;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `data` as `<root>/<unique>/<name>` and return the file's path.
pub(crate) async fn store(root: &Path, name: &str, data: &[u8]) -> Result<PathBuf, RpcError> {
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(RpcError::new(
            ErrorCode::InvalidArgument,
            format!(
                "file is {} bytes; the limit is {MAX_UPLOAD_BYTES}",
                data.len()
            ),
        ));
    }
    let io = |e: std::io::Error| RpcError::new(ErrorCode::Io, format!("upload: {e}"));
    let dir = root.join(format!(
        "{}-{}-{}",
        civet_proto::now_ms(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // The root may already exist (and, in a shared /tmp, may have been
    // planted by someone else): it must be ours, a real directory, 0700.
    let r = root.to_owned();
    tokio::task::spawn_blocking(move || crate::paths::create_private_dir(&r))
        .await
        .map_err(|e| RpcError::new(ErrorCode::Io, format!("upload: {e}")))?
        .map_err(io)?;
    // Fresh names only: never reuse an existing directory or file.
    let mut b = tokio::fs::DirBuilder::new();
    b.mode(0o700);
    b.create(&dir).await.map_err(io)?;
    let path = dir.join(sanitize(name));
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await
        .map_err(io)?;
    f.write_all(data).await.map_err(io)?;
    f.flush().await.map_err(io)?;
    Ok(path)
}

/// Default root: `$TMPDIR/civet-drops-<uid>`, one per user so a shared
/// `/tmp` (Linux) doesn't lock later users out of the first one's dir.
pub(crate) fn default_root() -> PathBuf {
    std::env::temp_dir().join(format!("civet-drops-{}", crate::paths::effective_uid()))
}

/// Basename only, no path separators, control bytes or leading dots.
fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let clean = clean.trim_start_matches('.').trim();
    if clean.is_empty() {
        "file".into()
    } else {
        clean.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_basename_only() {
        assert_eq!(sanitize("Screen Shot 1.png"), "Screen Shot 1.png");
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\x\\a.png"), "a.png");
        assert_eq!(sanitize(".."), "file");
        assert_eq!(sanitize("a\nb.png"), "a_b.png");
    }

    #[tokio::test]
    async fn stores_in_a_fresh_dir() {
        let root = std::env::temp_dir().join(format!("civet-upload-test-{}", std::process::id()));
        let a = store(&root, "a.png", b"one").await.unwrap();
        let b = store(&root, "a.png", b"two").await.unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(a.file_name().unwrap(), "a.png");
        let big = vec![0u8; MAX_UPLOAD_BYTES + 1];
        assert_eq!(
            store(&root, "big", &big).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refuses_a_symlinked_root() {
        let base = std::env::temp_dir().join(format!("civet-upload-link-{}", std::process::id()));
        let target = base.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let root = base.join("drops");
        std::os::unix::fs::symlink(&target, &root).unwrap();
        let err = store(&root, "a.png", b"x").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn tightens_an_existing_root() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("civet-upload-mode-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        store(&root, "a.png", b"x").await.unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_root_is_per_user() {
        let name = default_root()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            name,
            format!("civet-drops-{}", crate::paths::effective_uid())
        );
    }
}
