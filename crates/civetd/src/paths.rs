//! State-directory layout (`~/.civet` or `$CIVET_STATE_DIR`), shared by the
//! daemon and the clients. See `docs/INTERFACES.md` section 7.

use std::io::Write;
use std::path::{Path, PathBuf};

use civet_proto::ClientId;

/// Lock file held (flock) by the running daemon.
pub const LOCK_FILE: &str = "civetd.lock";
/// Running daemon's bootstrap info; written last when ready.
pub const DAEMON_JSON: &str = "daemon.json";
/// Unix socket for stdio-tunnel and local clients.
pub const SOCKET_FILE: &str = "civetd.sock";
/// Persisted client id (hex), client side.
pub const CLIENT_ID_FILE: &str = "client-id";

/// The state directory: `$CIVET_STATE_DIR` if set and non-empty, else
/// `~/.civet` (else `./.civet` when the home directory is unknown).
pub fn default_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(civet_proto::ENV_STATE_DIR).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(civet_proto::STATE_DIR_NAME)
}

/// `dir` if given, else [`default_state_dir`].
pub fn state_dir_or_default(dir: Option<&Path>) -> PathBuf {
    dir.map(Path::to_path_buf).unwrap_or_else(default_state_dir)
}

/// Paths inside one state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    /// The state directory itself.
    pub root: PathBuf,
}

impl StatePaths {
    /// Layout rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `civetd.lock`
    pub fn lock(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// `daemon.json`
    pub fn daemon_json(&self) -> PathBuf {
        self.root.join(DAEMON_JSON)
    }

    /// `civetd.sock`
    pub fn socket(&self) -> PathBuf {
        self.root.join(SOCKET_FILE)
    }

    /// `logs/`
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// `logs/civetd.log`
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir().join("civetd.log")
    }

    /// `bin/` (installed daemon binaries)
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// `ssh/` (client side: ssh ControlPath sockets)
    pub fn ssh_dir(&self) -> PathBuf {
        self.root.join("ssh")
    }

    /// `dist/` (client side: optional `civetd-<triple>` binaries)
    pub fn dist_dir(&self) -> PathBuf {
        self.root.join("dist")
    }

    /// `client-id`
    pub fn client_id(&self) -> PathBuf {
        self.root.join(CLIENT_ID_FILE)
    }
}

/// Create `dir` (and parents) and restrict it to the owner (0700).
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)
}

/// chmod.
pub fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Write `data` to `path` atomically (temp file in the same directory, then
/// rename) with the given mode.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Load the persisted client id from `<dir>/client-id`, or generate and save
/// one (mode 0600).
pub fn load_or_create_client_id(dir: &Path) -> std::io::Result<ClientId> {
    let paths = StatePaths::new(dir);
    let file = paths.client_id();
    if let Ok(s) = std::fs::read_to_string(&file)
        && let Ok(id) = ClientId::from_hex(s.trim())
    {
        return Ok(id);
    }
    let id = ClientId::generate().map_err(std::io::Error::from)?;
    create_private_dir(dir)?;
    write_atomic(&file, format!("{}\n", id.to_hex()).as_bytes(), 0o600)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_persists() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create_client_id(dir.path()).unwrap();
        let b = load_or_create_client_id(dir.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn atomic_write_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        write_atomic(&p, b"{}", 0o600).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{}");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
