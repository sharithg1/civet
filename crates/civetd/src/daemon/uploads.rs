//! `UploadFile`: files dropped on a pane in the desktop app (e.g. screenshots
//! for an agent) land here so a remote pane can reference them by path. Each
//! upload gets its own directory under `$TMPDIR/civet-drops/` and keeps its
//! original (sanitized) file name, which agents show to the user.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use civet_proto::{ErrorCode, MAX_UPLOAD_BYTES, RpcError};

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
    create_private_dir(root).await.map_err(io)?;
    create_private_dir(&dir).await.map_err(io)?;
    let path = dir.join(sanitize(name));
    tokio::fs::write(&path, data).await.map_err(io)?;
    Ok(path)
}

/// Default root: `$TMPDIR/civet-drops`.
pub(crate) fn default_root() -> PathBuf {
    std::env::temp_dir().join("civet-drops")
}

async fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    b.mode(0o700);
    match b.create(dir).await {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        r => r,
    }
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
}
