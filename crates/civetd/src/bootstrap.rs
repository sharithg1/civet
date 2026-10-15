//! Client side of bootstrapping (available with `default-features = false`).
//!
//! Like mosh: use the system `ssh` (so `~/.ssh/config`, agents, ProxyJump
//! and ControlMaster all behave as the user expects) to
//!
//! 1. **probe** the remote (`uname`, `$HOME`, whether
//!    `~/.civet/bin/civetd-<our version>` exists, is ours, and its
//!    `version` really prints our version; if so, run its `bootstrap` in the
//!    same round trip; a stale, truncated or wrong-arch file is replaced),
//! 2. **upload** the matching static binary if missing (version-suffixed, so
//!    a running daemon's file is never overwritten). The local file is
//!    checked first (embedded version marker, executable format and
//!    architecture: [`BootstrapError::WrongBinary`]), and the uploaded copy
//!    must print the right `version` on the remote before it is moved into
//!    place ([`BootstrapError::UploadRejected`]),
//! 3. **start** it (`civetd bootstrap` starts or reattaches the daemon and
//!    prints one JSON line),
//! 4. **resolve** the address to dial QUIC at (`ssh -G` → `hostname`).
//!
//! SSH is then done; it is used again only for the UDP-blocked fallback
//! ([`open_ssh_tunnel`]: the same framed protocol over
//! `ssh host civetd stdio-tunnel`).
//!
//! Every ssh call runs with `BatchMode=yes` (never prompts: the desktop has
//! no terminal; key or agent auth is required) and a shared ControlMaster
//! socket in `~/.civet/ssh/` so the calls (and a later tunnel) reuse one SSH
//! login. The destination is passed as a separate argv element after `--`
//! (no local shell is involved, and a destination starting with `-` is
//! refused), and the remote command is a POSIX `sh -c '<script>'` whose
//! user-supplied parts are single-quoted, so the remote login shell (bash,
//! zsh, fish…) only ever sees one quoted word. Remote paths containing
//! characters that some login shells interpret even inside single quotes
//! (`!` for csh/tcsh history, `\` for fish) or quotes and control
//! characters are refused ([`BootstrapError::InvalidRemotePath`]), and the
//! scripts refuse a remote state dir (or its `bin/`) that is a symlink or
//! not owned by the login user ([`BootstrapError::UnsafeRemoteDir`]).
//!
//! The ControlPath is quoted for ssh's config parser (spaces) with `%`
//! escaped; when the socket path would exceed the platform's unix socket
//! limit, connection sharing is turned off for the call (with a warning)
//! instead of failing every ssh call.
//!
//! # Building the binaries to upload
//!
//! The client looks for `civetd-<target-triple>` (see
//! [`BinarySource::from_env`] for the search order: an app-bundled
//! `civet-dist` resource dir, `$CIVET_DIST_DIR`, `~/.civet/dist`, then the
//! `civetd` next to the running executable). `scripts/build-remote.sh
//! --install` builds and installs the Linux ones.
//! Linux remotes want static musl binaries. They cross-compile from macOS
//! with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)
//! (optional; not needed for local development):
//!
//! ```sh
//! brew install zig && cargo install cargo-zigbuild
//! rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
//! for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
//!   cargo zigbuild -p civetd --bin civetd --profile dist --target $t
//!   cp target/$t/dist/civetd ~/.civet/dist/civetd-$t
//! done
//! ```
//!
//! For a remote with the same OS and architecture as the client,
//! [`BinarySource::CurrentExeSibling`] uploads the `civetd` next to the
//! running executable.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use std::time::Duration;

use civet_proto::BootstrapInfo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::paths::{StatePaths, create_private_dir, default_state_dir};

/// `civetd` process exit codes (INTERFACES.md 5.1).
pub mod exit_code {
    /// Success.
    pub const OK: i32 = 0;
    /// Generic error.
    pub const ERROR: i32 = 1;
    /// No daemon is running (stdio-tunnel).
    pub const NO_DAEMON: i32 = 2;
    /// Another daemon holds the lock (serve).
    pub const ALREADY_RUNNING: i32 = 3;
    /// An incompatible daemon with live panes is running (bootstrap).
    pub const INCOMPATIBLE_BUSY: i32 = 4;
}

/// Where to bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshTarget {
    /// `host`, `user@host` or an ssh_config alias.
    pub destination: String,
    /// SSH port (`-p`), if not the configured one.
    pub port: Option<u16>,
    /// Extra ssh args, e.g. `["-i", "~/.ssh/id_ed25519", "-J", "bastion"]`.
    pub extra_args: Vec<String>,
}

impl SshTarget {
    /// Destination with default port and no extra args.
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            port: None,
            extra_args: Vec::new(),
        }
    }
}

/// Where the daemon binary to upload comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinarySource {
    /// Directory with `civetd-<target-triple>` files (desktop app resources,
    /// or `CIVET_DIST_DIR`).
    Dir(PathBuf),
    /// Exactly this binary (must match the remote's triple).
    File(PathBuf),
    /// `civetd` next to the current executable (dev: same-OS remotes only).
    CurrentExeSibling,
    /// Try these in order; the first one that has a file for the remote's
    /// triple with the right embedded version wins (a stale file is skipped
    /// with a warning; if every candidate is stale the first one's
    /// [`BootstrapError::WrongBinary`] is returned).
    Search(Vec<BinarySource>),
}

/// Name of the directory of `civetd-<triple>` files an app bundle ships
/// (macOS: `<App>.app/Contents/Resources/civet-dist`; elsewhere next to
/// the executable).
pub const BUNDLED_DIST_DIR: &str = "civet-dist";

impl BinarySource {
    /// The default search order: an app-bundled [`BUNDLED_DIST_DIR`] (if
    /// present), `$CIVET_DIST_DIR` (if set), `~/.civet/dist` (if present),
    /// then [`BinarySource::CurrentExeSibling`].
    pub fn from_env() -> Self {
        Self::with_bundled(bundled_dist_dir())
    }

    /// Like [`BinarySource::from_env`] with an explicit bundled resource dir
    /// (e.g. Tauri's resource directory) searched first.
    pub fn with_bundled(bundled: Option<PathBuf>) -> Self {
        let mut chain = Vec::new();
        if let Some(d) = bundled.filter(|d| d.is_dir()) {
            chain.push(Self::Dir(d));
        }
        if let Some(d) = std::env::var_os("CIVET_DIST_DIR").filter(|d| !d.is_empty()) {
            chain.push(Self::Dir(PathBuf::from(d)));
        }
        let dist = StatePaths::new(default_state_dir()).dist_dir();
        if dist.is_dir() {
            chain.push(Self::Dir(dist));
        }
        chain.push(Self::CurrentExeSibling);
        Self::Search(chain)
    }

    /// Short description for logs and errors.
    pub fn describe(&self) -> String {
        match self {
            Self::Dir(d) => format!("dir {}", d.display()),
            Self::File(f) => format!("file {}", f.display()),
            Self::CurrentExeSibling => "civetd next to the running executable".into(),
            Self::Search(v) => v
                .iter()
                .map(Self::describe)
                .collect::<Vec<_>>()
                .join(", then "),
        }
    }
}

/// `<exe dir>/../Resources/civet-dist` (macOS app bundle) or
/// `<exe dir>/civet-dist`, if either exists.
fn bundled_dist_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [
        dir.join("../Resources").join(BUNDLED_DIST_DIR),
        dir.join(BUNDLED_DIST_DIR),
    ]
    .into_iter()
    .find(|d| d.is_dir())
}

/// Options for [`bootstrap_ssh`] / [`open_ssh_tunnel`].
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    /// The host.
    pub target: SshTarget,
    /// ssh binary, normally `"ssh"`.
    pub ssh_program: PathBuf,
    /// Binary to upload if the remote lacks this version.
    pub binary: BinarySource,
    /// Remote state dir; None = remote default (~/.civet). `~/` is expanded
    /// on the remote.
    pub remote_state_dir: Option<String>,
    /// Pass `--replace` to the remote bootstrap.
    pub replace: bool,
    /// Per ssh invocation. Default 30 s.
    pub timeout: Duration,
}

impl BootstrapOptions {
    /// Defaults: `ssh`, [`BinarySource::from_env`], remote default state
    /// dir, no replace, 30 s.
    pub fn new(target: SshTarget) -> Self {
        Self {
            target,
            ssh_program: PathBuf::from("ssh"),
            binary: BinarySource::from_env(),
            remote_state_dir: None,
            replace: false,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Result of a successful bootstrap.
#[derive(Debug, Clone)]
pub struct Bootstrapped {
    /// The daemon's bootstrap line.
    pub info: BootstrapInfo,
    /// IP to dial for QUIC: `hostname` from `ssh -G`, resolved.
    pub dial_addr: SocketAddr,
    /// Remote path of the civetd binary (for stdio-tunnel).
    pub remote_binary: String,
    /// True if this call uploaded a binary.
    pub installed: bool,
}

/// Bootstrap failures.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// ssh (or the remote command) failed.
    #[error("ssh failed (exit {code:?}): {stderr}")]
    Ssh {
        /// Exit code, None if killed by a signal.
        code: Option<i32>,
        /// stderr, trimmed, plus a hint for common failures.
        stderr: String,
    },
    /// No binary for the remote platform.
    #[error("no civetd binary for {triple} (looked in {searched})")]
    NoBinary {
        /// Remote target triple.
        triple: String,
        /// Where we looked.
        searched: String,
    },
    /// `uname` reported something we have no binaries for.
    #[error("unsupported remote platform: {0}")]
    UnsupportedPlatform(String),
    /// Remote bootstrap exited 4.
    #[error("remote daemon is incompatible and has live panes; retry with replace")]
    IncompatibleDaemonBusy,
    /// Bad bootstrap output.
    #[error(transparent)]
    Proto(#[from] civet_proto::ProtoError),
    /// Local I/O (spawning ssh, reading the binary, local civetd failures).
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// An ssh call exceeded [`BootstrapOptions::timeout`].
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// The local `civetd-<triple>` file is not this client's version (or
    /// not an executable for the remote's platform). Nothing was uploaded.
    #[error(
        "{path} is civetd {got}, expected {expected}: rebuild it with \
         `scripts/build-remote.sh --install` (or point CIVET_DIST_DIR at a matching build)"
    )]
    WrongBinary {
        /// The local file.
        path: String,
        /// This client's version (and platform).
        expected: String,
        /// What the file is.
        got: String,
    },
    /// The uploaded binary did not print the expected `version` on the
    /// remote (truncated upload, wrong architecture, missing loader...). It
    /// was removed again.
    #[error("uploaded civetd failed its check on the remote (expected civetd {expected}): {got}")]
    UploadRejected {
        /// Expected version.
        expected: String,
        /// What `civetd version` printed (or why it failed).
        got: String,
    },
    /// The remote state dir or its `bin/` is a symlink or not owned by the
    /// login user.
    #[error("unsafe remote state directory: {0}")]
    UnsafeRemoteDir(String),
    /// A remote path contains characters the remote login shell could
    /// misinterpret (`!`, `\`, quotes, control characters).
    #[error("unsupported remote path {0:?}: it must not contain ! \\ ' \" ` or control characters")]
    InvalidRemotePath(String),
}

/// Progress callback steps (the desktop shows them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStep {
    /// Probing the remote (and bootstrapping if the binary is there).
    Probe,
    /// Uploading the daemon binary.
    Upload,
    /// Starting the daemon.
    Start,
    /// Resolving the dial address.
    Resolve,
}

/// Version marker embedded in the `civetd` binary (a `#[used]` static in
/// `main.rs`): `\0CIVETD_VERSION=<version>\0`.
pub const VERSION_MARKER: &str = concat!("\0CIVETD_VERSION=", env!("CARGO_PKG_VERSION"), "\0");

/// [`VERSION_MARKER`] as a byte array (for the static in `main.rs`).
pub const fn version_marker_bytes() -> [u8; VERSION_MARKER.len()] {
    let src = VERSION_MARKER.as_bytes();
    let mut out = [0u8; VERSION_MARKER.len()];
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
}

/// The version embedded in a `civetd` binary (its [`VERSION_MARKER`]), or
/// None if the file has no marker (a build from before markers existed, or
/// not civetd at all).
pub fn binary_version(data: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"\0CIVETD_VERSION=";
    let start = data.windows(PREFIX.len()).position(|w| w == PREFIX)? + PREFIX.len();
    let len = data[start..].iter().take(64).position(|&b| b == 0)?;
    let v = std::str::from_utf8(&data[start..start + len]).ok()?;
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_graphic())).then(|| v.to_owned())
}

/// (os, arch) of an executable from its header: ELF (`linux`) or Mach-O
/// (`darwin`, `None` arch for universal binaries).
fn executable_platform(data: &[u8]) -> Option<(&'static str, Option<&'static str>)> {
    if data.len() >= 20 && data[..4] == [0x7f, b'E', b'L', b'F'] {
        let machine = if data[5] == 2 {
            u16::from_be_bytes([data[18], data[19]])
        } else {
            u16::from_le_bytes([data[18], data[19]])
        };
        let arch = match machine {
            62 => Some("x86_64"),
            183 => Some("aarch64"),
            _ => Some("other"),
        };
        return Some(("linux", arch));
    }
    if data.len() >= 8 {
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic == 0xfeed_facf {
            let cpu = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let arch = match cpu {
                0x0100_0007 => Some("x86_64"),
                0x0100_000c => Some("aarch64"),
                _ => Some("other"),
            };
            return Some(("darwin", arch));
        }
        if data[..4] == [0xca, 0xfe, 0xba, 0xbe] {
            return Some(("darwin", None));
        }
    }
    None
}

/// Check a local `civetd` file before uploading it for `triple`: it must
/// carry this client's [`VERSION_MARKER`] and be an executable for the
/// triple's OS and architecture.
pub fn check_binary(path: &Path, data: &[u8], triple: &str) -> Result<(), BootstrapError> {
    let wrong = |got: String| BootstrapError::WrongBinary {
        path: path.display().to_string(),
        expected: format!("{} for {triple}", crate::VERSION),
        got,
    };
    match binary_version(data) {
        Some(v) if v == crate::VERSION => {}
        Some(v) => return Err(wrong(v)),
        None => return Err(wrong("of an unknown version (no version marker)".into())),
    }
    let want_arch = triple.split('-').next().unwrap_or("");
    let want_os = if triple.contains("linux") {
        "linux"
    } else if triple.contains("darwin") {
        "darwin"
    } else {
        "other"
    };
    match executable_platform(data) {
        Some((os, arch)) if os == want_os && arch.is_none_or(|a| a == want_arch) => Ok(()),
        Some((os, arch)) => Err(wrong(format!(
            "{} built for {os} {}",
            crate::VERSION,
            arch.unwrap_or("?")
        ))),
        None => Err(wrong(format!(
            "{} but not an executable for {triple}",
            crate::VERSION
        ))),
    }
}

/// Map `uname -s` + `uname -m` to a target triple.
pub fn target_triple(uname_s: &str, uname_m: &str) -> Result<String, BootstrapError> {
    let os = uname_s.trim();
    let arch = match uname_m.trim() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => {
            return Err(BootstrapError::UnsupportedPlatform(format!("{os} {other}")));
        }
    };
    match os {
        "Linux" => Ok(format!("{arch}-unknown-linux-musl")),
        "Darwin" => Ok(format!("{arch}-apple-darwin")),
        other => Err(BootstrapError::UnsupportedPlatform(format!(
            "{other} {arch}"
        ))),
    }
}

/// Quote `s` as one POSIX shell word.
pub fn sh_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./=:,@%+-".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Refuse remote paths with characters a remote login shell may interpret
/// even inside our single quotes: `!` (csh/tcsh history expansion), `\`
/// (fish), quotes, backquotes, control characters (a newline ends a csh
/// command). Such paths are never needed in practice.
fn check_remote_path(path: &str) -> Result<(), BootstrapError> {
    let bad = path.is_empty()
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '!' | '\\' | '\'' | '"' | '`'));
    if bad {
        Err(BootstrapError::InvalidRemotePath(path.to_owned()))
    } else {
        Ok(())
    }
}

/// Shell expression for a remote path; a leading `~/` becomes `"$HOME"/`.
fn remote_path_expr(path: &str) -> Result<String, BootstrapError> {
    check_remote_path(path)?;
    Ok(if path == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest))
    } else {
        sh_quote(path)
    })
}

/// Remote exit code: the uploaded binary failed its `version` check.
const EXIT_UPLOAD_REJECTED: i32 = 86;
/// Remote exit code: the state dir or bin/ is a symlink / not ours.
const EXIT_UNSAFE_DIR: i32 = 87;

/// POSIX sh function `ok DIR`: DIR is a real directory (not a symlink)
/// owned by the login user. No `!` (csh history) in it.
const SH_DIR_OK: &str =
    "ok() { if [ -L \"$1\" ]; then return 1; fi; [ -d \"$1\" ] && [ -O \"$1\" ]; }; ";

/// Unix socket path limit (`sun_path`, including the NUL).
#[cfg(target_os = "linux")]
const SUN_PATH_MAX: usize = 108;
#[cfg(not(target_os = "linux"))]
const SUN_PATH_MAX: usize = 104;

/// `ControlPath=...` for ssh's config parser (double-quoted for spaces,
/// `%` escaped), or why it cannot work: ssh's socket is `<dir>/<40 hex>`
/// and while the master starts it binds `<that>.<16 random chars>`, which
/// must fit `sun_path`.
fn control_path_option(ssh_dir: &Path) -> Result<String, String> {
    let dir = ssh_dir.to_string_lossy();
    if dir.contains(['"', '\'']) || dir.chars().any(char::is_control) {
        return Err(format!("{dir} contains quotes or control characters"));
    }
    let len = dir.len() + 1 + 40 + 17 + 1;
    if len > SUN_PATH_MAX {
        return Err(format!(
            "the ssh control socket path in {dir} would be {len} bytes, over the \
             {SUN_PATH_MAX}-byte unix socket limit (use a shorter CIVET_STATE_DIR)"
        ));
    }
    Ok(format!("ControlPath=\"{}/%C\"", dir.replace('%', "%%")))
}

impl BootstrapOptions {
    /// Shell expression for the remote state dir.
    fn state_expr(&self) -> Result<String, BootstrapError> {
        remote_path_expr(self.remote_state_dir.as_deref().unwrap_or("~/.civet"))
    }

    /// `civetd bootstrap` arguments (already quoted).
    fn bootstrap_args(&self) -> Result<String, BootstrapError> {
        let mut s = String::from("bootstrap");
        if let Some(d) = &self.remote_state_dir {
            s.push_str(" --state-dir ");
            s.push_str(&remote_path_expr(d)?);
        }
        if self.replace {
            s.push_str(" --replace");
        }
        Ok(s)
    }

    /// Base ssh arguments up to and including the destination.
    fn ssh_args(&self, batch: bool) -> Result<Vec<OsString>, BootstrapError> {
        let dest = &self.target.destination;
        if dest.is_empty() || dest.starts_with('-') {
            return Err(BootstrapError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid ssh destination {dest:?}"),
            )));
        }
        let mut args: Vec<OsString> = Vec::new();
        if batch {
            let ssh_dir = StatePaths::new(default_state_dir()).ssh_dir();
            create_private_dir(&ssh_dir)?;
            let mut opts = vec!["BatchMode=yes".to_string()];
            match control_path_option(&ssh_dir) {
                Ok(cp) => {
                    opts.push("ControlMaster=auto".into());
                    opts.push(cp);
                    opts.push("ControlPersist=60".into());
                }
                Err(why) => {
                    tracing::warn!("ssh connection sharing disabled: {why}");
                    opts.push("ControlMaster=no".into());
                    opts.push("ControlPath=none".into());
                }
            }
            for opt in opts {
                args.push("-o".into());
                args.push(opt.into());
            }
        }
        if let Some(port) = self.target.port {
            args.push("-p".into());
            args.push(port.to_string().into());
        }
        args.extend(self.target.extra_args.iter().map(OsString::from));
        args.push("--".into());
        args.push(dest.into());
        Ok(args)
    }
}

/// Output of one ssh call.
#[derive(Debug)]
struct SshOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn hint(stderr: &str) -> String {
    let s = stderr.trim();
    let h = if s.contains("Permission denied") {
        " (hint: civet needs key or agent auth; check `ssh -o BatchMode=yes <host> true`)"
    } else if s.contains("Host key verification failed") {
        " (hint: connect once with plain `ssh <host>` to accept the host key)"
    } else {
        ""
    };
    format!("{s}{h}")
}

/// Run `ssh <args> -- <dest> sh -c '<script>'`, optionally feeding stdin.
async fn run_ssh(
    opts: &BootstrapOptions,
    script: &str,
    stdin_data: Option<Vec<u8>>,
    what: &'static str,
) -> Result<SshOutput, BootstrapError> {
    let mut cmd = Command::new(&opts.ssh_program);
    cmd.args(opts.ssh_args(true)?)
        .arg(format!("sh -c {}", sh_quote(script)))
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    run_child(cmd, stdin_data, opts.timeout, what).await
}

async fn run_child(
    mut cmd: Command,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
    what: &'static str,
) -> Result<SshOutput, BootstrapError> {
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take();
    let fut = async move {
        let writer = async move {
            if let (Some(mut stdin), Some(data)) = (stdin, stdin_data) {
                stdin.write_all(&data).await?;
                stdin.shutdown().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let (w, out) = tokio::join!(writer, child.wait_with_output());
        let out = out?;
        // A failed stdin write shows up as a failed remote command; report
        // the exit status rather than EPIPE.
        if let Err(e) = w
            && out.status.success()
        {
            return Err(BootstrapError::Io(e));
        }
        Ok(SshOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| BootstrapError::Timeout(what))?
}

/// Parsed probe output.
#[derive(Debug, Default)]
struct ProbeResult {
    uname: Option<(String, String)>,
    home: Option<String>,
    have_binary: bool,
    bootstrap_exit: Option<i32>,
    /// `version` output of a present but mismatching remote binary.
    stale: Option<String>,
}

fn parse_probe(stdout: &str) -> ProbeResult {
    let mut r = ProbeResult::default();
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("CIVET_UNAME=") {
            let mut it = rest.split_whitespace();
            if let (Some(s), Some(m)) = (it.next(), it.next()) {
                r.uname = Some((s.to_string(), m.to_string()));
            }
        } else if let Some(rest) = line.strip_prefix("CIVET_HOME=") {
            r.home = Some(rest.to_string());
        } else if line == "CIVET_HAVE=1" {
            r.have_binary = true;
        } else if let Some(rest) = line.strip_prefix("CIVET_EXIT=") {
            r.bootstrap_exit = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("CIVET_STALE=") {
            r.stale = Some(rest.to_string());
        }
    }
    r
}

/// Remote binary path expression (shell) for our version.
fn bin_expr(opts: &BootstrapOptions) -> Result<String, BootstrapError> {
    Ok(format!(
        "{}/bin/civetd-{}",
        opts.state_expr()?,
        crate::VERSION
    ))
}

/// The probe script: platform, `$HOME`, and, if `bin/civetd-<ver>` exists in
/// an owned (non-symlink) state dir and its `version` prints exactly our
/// version, its `bootstrap` output in the same round trip. A stale, broken
/// or foreign binary is reported as `CIVET_HAVE=0` (+ `CIVET_STALE=`), so
/// it gets reinstalled.
fn probe_script(opts: &BootstrapOptions) -> Result<String, BootstrapError> {
    Ok(format!(
        "echo \"CIVET_UNAME=$(uname -s) $(uname -m)\"; echo \"CIVET_HOME=$HOME\"; {ok}\
         S={state}; B=\"$S/bin/civetd-{ver}\"; V=\"\"; \
         if ok \"$S\" && ok \"$S/bin\" && [ -f \"$B\" ] && [ -x \"$B\" ] && [ -O \"$B\" ]; then \
         V=$(\"$B\" version 2>/dev/null) || V=\"\"; fi; \
         case \"$V\" in \"civetd {ver} \"*) echo CIVET_HAVE=1; \"$B\" {args}; echo \"CIVET_EXIT=$?\" ;; \
         *) echo CIVET_HAVE=0; echo \"CIVET_STALE=$V\" ;; esac",
        ok = SH_DIR_OK,
        state = opts.state_expr()?,
        ver = crate::VERSION,
        args = opts.bootstrap_args()?,
    ))
}

/// The upload script (binary on stdin): refuse an unsafe state dir (exit
/// 87), write a temp file, require its `version` to print our version (else
/// remove it, exit 86), then move it into place.
fn upload_script(opts: &BootstrapOptions) -> Result<String, BootstrapError> {
    Ok(format!(
        "umask 077; {ok}S={state}; \
         for d in \"$S\" \"$S/bin\"; do mkdir -p \"$d\" 2>/dev/null; if ok \"$d\"; then :; else \
         echo \"civet: $d is a symlink or not a directory owned by $(id -un)\" >&2; exit {unsafe_dir}; fi; done; \
         T=\"$S/bin/.civetd-{ver}.tmp.$$\"; \
         if cat > \"$T\"; then :; else rm -f \"$T\"; exit 1; fi; chmod 755 \"$T\"; \
         V=$(\"$T\" version 2>&1) || V=\"exit $?: $V\"; \
         case \"$V\" in \"civetd {ver} \"*) ;; *) rm -f \"$T\"; echo \"CIVET_BADBIN=$V\" >&2; exit {rejected} ;; esac; \
         mv -f \"$T\" \"$S/bin/civetd-{ver}\" && ln -sf civetd-{ver} \"$S/bin/civetd\"",
        ok = SH_DIR_OK,
        state = opts.state_expr()?,
        ver = crate::VERSION,
        unsafe_dir = EXIT_UNSAFE_DIR,
        rejected = EXIT_UPLOAD_REJECTED,
    ))
}

/// Map an upload script failure to an error.
fn upload_error(out: &SshOutput) -> BootstrapError {
    match out.code {
        Some(EXIT_UPLOAD_REJECTED) => BootstrapError::UploadRejected {
            expected: crate::VERSION.to_owned(),
            got: out
                .stderr
                .lines()
                .find_map(|l| l.strip_prefix("CIVET_BADBIN="))
                .unwrap_or("no output")
                .trim()
                .to_owned(),
        },
        Some(EXIT_UNSAFE_DIR) => BootstrapError::UnsafeRemoteDir(out.stderr.trim().to_owned()),
        code => BootstrapError::Ssh {
            code,
            stderr: hint(&out.stderr),
        },
    }
}

/// Turn a remote bootstrap exit code + output into a result.
fn bootstrap_result(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<BootstrapInfo, BootstrapError> {
    match code {
        Some(0) => Ok(BootstrapInfo::find_in_output(stdout)?),
        Some(exit_code::INCOMPATIBLE_BUSY) => Err(BootstrapError::IncompatibleDaemonBusy),
        code => Err(BootstrapError::Ssh {
            code,
            stderr: hint(stderr),
        }),
    }
}

/// Bootstrap over SSH (see the module docs).
pub async fn bootstrap_ssh(
    opts: &BootstrapOptions,
    progress: &(dyn Fn(BootstrapStep) + Send + Sync),
) -> Result<Bootstrapped, BootstrapError> {
    // 1. Probe (+ bootstrap if the binary is present), one round trip.
    progress(BootstrapStep::Probe);
    let out = run_ssh(opts, &probe_script(opts)?, None, "ssh probe").await?;
    let probe = parse_probe(&out.stdout);
    let Some((uname_s, uname_m)) = probe.uname.clone() else {
        return Err(BootstrapError::Ssh {
            code: out.code,
            stderr: hint(&out.stderr),
        });
    };
    let home = probe.home.clone().unwrap_or_default();
    let remote_binary = absolute_remote_binary(opts, &home);
    check_remote_path(&remote_binary)?;

    let mut installed = false;
    let info = match (probe.have_binary, probe.bootstrap_exit) {
        // 126/127: present but not runnable after all: reinstall.
        (true, Some(code)) if code != 126 && code != 127 => {
            bootstrap_result(Some(code), &out.stdout, &out.stderr)?
        }
        _ => {
            if let Some(stale) = probe.stale.as_deref().filter(|s| !s.is_empty()) {
                tracing::info!(
                    remote = %remote_binary,
                    found = stale,
                    "remote civetd binary is not {}; reinstalling",
                    crate::VERSION
                );
            }
            // 2. Upload.
            progress(BootstrapStep::Upload);
            let triple = target_triple(&uname_s, &uname_m)?;
            let (local, data) = find_binary(&opts.binary, &triple).await?;
            tracing::info!(
                triple,
                path = %local.display(),
                source = %opts.binary.describe(),
                "uploading civetd {}",
                crate::VERSION
            );
            let up = run_ssh(opts, &upload_script(opts)?, Some(data), "ssh upload").await?;
            if up.code != Some(0) {
                return Err(upload_error(&up));
            }
            installed = true;
            // 3. Start.
            progress(BootstrapStep::Start);
            let start_script = format!("exec {} {}", bin_expr(opts)?, opts.bootstrap_args()?);
            let st = run_ssh(opts, &start_script, None, "ssh bootstrap").await?;
            bootstrap_result(st.code, &st.stdout, &st.stderr)?
        }
    };

    // 4. Resolve.
    progress(BootstrapStep::Resolve);
    let host = ssh_hostname(opts).await?;
    let dial_addr = resolve(&host, info.udp_port).await?;
    Ok(Bootstrapped {
        info,
        dial_addr,
        remote_binary,
        installed,
    })
}

fn absolute_remote_binary(opts: &BootstrapOptions, home: &str) -> String {
    let state = match opts.remote_state_dir.as_deref() {
        None => format!("{home}/.civet"),
        Some("~") => home.to_string(),
        Some(d) => match d.strip_prefix("~/") {
            Some(rest) => format!("{home}/{rest}"),
            None => d.to_string(),
        },
    };
    format!("{state}/bin/civetd-{}", crate::VERSION)
}

/// Locate the binary to upload for `triple` and check it
/// ([`check_binary`]); returns its path and contents.
async fn find_binary(
    source: &BinarySource,
    triple: &str,
) -> Result<(PathBuf, Vec<u8>), BootstrapError> {
    let mut searched = Vec::new();
    let mut wrong = None;
    for candidate in flatten(source) {
        let path = match candidate_path(candidate, triple) {
            Ok(p) => p,
            Err(why) => {
                searched.push(why);
                continue;
            }
        };
        let data = tokio::fs::read(&path).await?;
        match check_binary(&path, &data, triple) {
            Ok(()) => return Ok((path, data)),
            Err(e) => {
                tracing::warn!("skipping {}: {e}", path.display());
                searched.push(format!("{} (wrong version)", path.display()));
                wrong.get_or_insert(e);
            }
        }
    }
    Err(wrong.unwrap_or_else(|| BootstrapError::NoBinary {
        triple: triple.to_string(),
        searched: searched.join(", "),
    }))
}

fn flatten(source: &BinarySource) -> Vec<&BinarySource> {
    match source {
        BinarySource::Search(v) => v.iter().flat_map(flatten).collect(),
        other => vec![other],
    }
}

/// The file a single (non-Search) source names for `triple`, or why not.
fn candidate_path(source: &BinarySource, triple: &str) -> Result<PathBuf, String> {
    let p = match source {
        BinarySource::Dir(dir) => dir.join(format!("civetd-{triple}")),
        BinarySource::File(p) => p.clone(),
        BinarySource::CurrentExeSibling => {
            let p = sibling_civetd().map_err(|e| format!("civetd next to this program ({e})"))?;
            if !same_platform(crate::TARGET, triple) {
                return Err(format!(
                    "{} (built for {}, set CIVET_DIST_DIR for other targets)",
                    p.display(),
                    crate::TARGET
                ));
            }
            p
        }
        BinarySource::Search(_) => return Err("nested search".into()),
    };
    if p.is_file() {
        Ok(p)
    } else {
        Err(p.display().to_string())
    }
}

/// Same architecture and OS family (a local gnu build runs on a musl
/// remote's kernel just as well for dev purposes).
fn same_platform(local: &str, remote: &str) -> bool {
    let key = |t: &str| {
        let arch = t.split('-').next().unwrap_or("").to_string();
        let os = if t.contains("linux") {
            "linux"
        } else if t.contains("darwin") {
            "darwin"
        } else {
            "other"
        };
        (arch, os)
    };
    key(local) == key(remote)
}

/// `civetd` next to the current executable.
pub fn sibling_civetd() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("current executable has no directory"))?;
    Ok(dir.join("civetd"))
}

/// `ssh -G` → `hostname`.
async fn ssh_hostname(opts: &BootstrapOptions) -> Result<String, BootstrapError> {
    let mut cmd = Command::new(&opts.ssh_program);
    cmd.arg("-G")
        .args(opts.ssh_args(false)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = run_child(cmd, None, opts.timeout, "ssh -G").await?;
    if out.code != Some(0) {
        return Err(BootstrapError::Ssh {
            code: out.code,
            stderr: hint(&out.stderr),
        });
    }
    out.stdout
        .lines()
        .find_map(|l| l.strip_prefix("hostname "))
        .map(|h| h.trim().to_string())
        .ok_or_else(|| BootstrapError::Ssh {
            code: out.code,
            stderr: "ssh -G printed no hostname".into(),
        })
}

/// Resolve `host:port`, preferring IPv4.
async fn resolve(host: &str, port: u16) -> Result<SocketAddr, BootstrapError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .ok_or_else(|| {
            BootstrapError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{host} did not resolve"),
            ))
        })
}

/// Run `<civetd> bootstrap --bind 127.0.0.1 [--state-dir]` as a local
/// child (no ssh); dial address 127.0.0.1. Used by `civet-cli --local` and
/// desktop "Local". A daemon it starts listens on loopback only.
pub async fn bootstrap_local(
    civetd: &Path,
    state_dir: Option<&Path>,
) -> Result<Bootstrapped, BootstrapError> {
    bootstrap_local_with(civetd, state_dir, false).await
}

/// [`bootstrap_local`] with `replace` (`--replace`: restart a running
/// daemon even if it has live panes, e.g. after `upgrade_available`).
pub async fn bootstrap_local_with(
    civetd: &Path,
    state_dir: Option<&Path>,
    replace: bool,
) -> Result<Bootstrapped, BootstrapError> {
    let mut cmd = Command::new(civetd);
    cmd.arg("bootstrap").arg("--bind").arg("127.0.0.1");
    if let Some(d) = state_dir {
        cmd.arg("--state-dir").arg(d);
    }
    if replace {
        cmd.arg("--replace");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = run_child(cmd, None, Duration::from_secs(30), "civetd bootstrap").await?;
    let info = match out.code {
        Some(0) => BootstrapInfo::find_in_output(&out.stdout)?,
        Some(exit_code::INCOMPATIBLE_BUSY) => return Err(BootstrapError::IncompatibleDaemonBusy),
        code => {
            return Err(BootstrapError::Io(std::io::Error::other(format!(
                "{} bootstrap exited with {code:?}: {}",
                civetd.display(),
                out.stderr.trim()
            ))));
        }
    };
    Ok(Bootstrapped {
        dial_addr: SocketAddr::from(([127, 0, 0, 1], info.udp_port)),
        info,
        remote_binary: civetd.display().to_string(),
        installed: false,
    })
}

/// A child process's stdin/stdout as one byte stream (the child is killed
/// when this is dropped). Returned by [`open_ssh_tunnel`] and
/// [`open_local_tunnel`].
#[derive(Debug)]
pub struct SshTunnel {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl SshTunnel {
    fn spawn(mut cmd: Command) -> Result<Self, BootstrapError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("no stdout"))?;
        if let Some(mut stderr) = child.stderr.take() {
            // Drain stderr so a chatty ssh can never block on a full pipe.
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = stderr.read_to_end(&mut buf).await;
                let s = String::from_utf8_lossy(&buf);
                if !s.trim().is_empty() {
                    tracing::debug!(stderr = %s.trim(), "tunnel process stderr");
                }
            });
        }
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    /// The child's pid (None once reaped).
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

impl AsyncRead for SshTunnel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshTunnel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

/// Spawn `ssh <target> <remote_binary> stdio-tunnel` and return its
/// stdin/stdout joined as one byte stream (kill on drop). Reuses the
/// ControlMaster connection of the bootstrap when it is still up.
pub async fn open_ssh_tunnel(
    opts: &BootstrapOptions,
    remote_binary: &str,
) -> Result<SshTunnel, BootstrapError> {
    let mut script = format!("exec {} stdio-tunnel", remote_path_expr(remote_binary)?);
    if let Some(d) = &opts.remote_state_dir {
        script.push_str(" --state-dir ");
        script.push_str(&remote_path_expr(d)?);
    }
    let mut cmd = Command::new(&opts.ssh_program);
    cmd.args(opts.ssh_args(true)?)
        .arg(format!("sh -c {}", sh_quote(&script)));
    SshTunnel::spawn(cmd)
}

/// Spawn a local `<civetd> stdio-tunnel [--state-dir]` (the local
/// equivalent of [`open_ssh_tunnel`]; `civet-cli --local --stdio`).
pub async fn open_local_tunnel(
    civetd: &Path,
    state_dir: Option<&Path>,
) -> Result<SshTunnel, BootstrapError> {
    let mut cmd = Command::new(civetd);
    cmd.arg("stdio-tunnel");
    if let Some(d) = state_dir {
        cmd.arg("--state-dir").arg(d);
    }
    SshTunnel::spawn(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triples() {
        assert_eq!(
            target_triple("Linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("Linux", "aarch64\n").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("Darwin", "arm64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_triple("Darwin", "x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        assert!(matches!(
            target_triple("FreeBSD", "amd64"),
            Err(BootstrapError::UnsupportedPlatform(_))
        ));
        assert!(target_triple("Linux", "riscv64").is_err());
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("abc/def-1.2"), "abc/def-1.2");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(remote_path_expr("~/.civet").unwrap(), "\"$HOME\"/.civet");
        assert_eq!(remote_path_expr("~/my dir").unwrap(), "\"$HOME\"/'my dir'");
        assert_eq!(remote_path_expr("/tmp/x;y").unwrap(), "'/tmp/x;y'");
    }

    #[test]
    fn quoted_script_survives_sh() {
        // The remote command is `sh -c <quoted>`; check a nasty script
        // round-trips through a real sh unchanged.
        let script = "printf '%s|' \"it's\" '$HOME' \"$((1+1))\"";
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sh -c {}", sh_quote(script)))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "it's|$HOME|2|");
    }

    #[test]
    fn destination_is_not_an_option() {
        let mut opts = BootstrapOptions::new(SshTarget::new("-oProxyCommand=evil"));
        opts.binary = BinarySource::CurrentExeSibling;
        assert!(opts.ssh_args(false).is_err());
        let mut ok = BootstrapOptions::new(SshTarget {
            destination: "user@host".into(),
            port: Some(2222),
            extra_args: vec!["-J".into(), "bastion".into()],
        });
        ok.binary = BinarySource::CurrentExeSibling;
        let args: Vec<String> = ok
            .ssh_args(false)
            .unwrap()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["-p", "2222", "-J", "bastion", "--", "user@host"]);
    }

    #[test]
    fn probe_parsing_and_paths() {
        let out = "Welcome!\nCIVET_UNAME=Linux x86_64\nCIVET_HOME=/home/u\nCIVET_HAVE=1\n{\"x\":1}\nCIVET_EXIT=4\n";
        let p = parse_probe(out);
        assert_eq!(p.uname, Some(("Linux".into(), "x86_64".into())));
        assert_eq!(p.home.as_deref(), Some("/home/u"));
        assert!(p.have_binary);
        assert_eq!(p.bootstrap_exit, Some(4));
        assert!(matches!(
            bootstrap_result(Some(4), out, ""),
            Err(BootstrapError::IncompatibleDaemonBusy)
        ));
        let mut opts = BootstrapOptions::new(SshTarget::new("h"));
        assert_eq!(
            absolute_remote_binary(&opts, "/home/u"),
            format!("/home/u/.civet/bin/civetd-{}", crate::VERSION)
        );
        opts.remote_state_dir = Some("~/state".into());
        assert_eq!(
            absolute_remote_binary(&opts, "/home/u"),
            format!("/home/u/state/bin/civetd-{}", crate::VERSION)
        );
        assert!(
            opts.bootstrap_args()
                .unwrap()
                .contains("--state-dir \"$HOME\"/state")
        );
    }

    #[test]
    fn platform_match() {
        assert!(same_platform(
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl"
        ));
        assert!(!same_platform(
            "aarch64-apple-darwin",
            "x86_64-apple-darwin"
        ));
        assert!(!same_platform(
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-musl"
        ));
    }

    /// Paths with characters a csh/tcsh or fish login shell would interpret
    /// inside single quotes are refused; nothing else is.
    #[test]
    fn remote_paths_are_validated() {
        for bad in [
            "~/a!b", "~/a\\b", "/x/it's", "/x/\"q\"", "/x/`id`", "/x/a\nb", "/x/\t", "",
        ] {
            assert!(
                matches!(
                    remote_path_expr(bad),
                    Err(BootstrapError::InvalidRemotePath(_))
                ),
                "{bad:?}"
            );
        }
        for good in [
            "~",
            "~/.civet",
            "~/my dir",
            "/srv/civet-state_1.2",
            "/x/$HOME;rm",
        ] {
            assert!(remote_path_expr(good).is_ok(), "{good:?}");
        }
        let mut opts = BootstrapOptions::new(SshTarget::new("h"));
        opts.remote_state_dir = Some("~/x!y".into());
        assert!(probe_script(&opts).is_err());
        assert!(upload_script(&opts).is_err());
    }

    /// The remote command (`sh -c '<script>'`) must survive csh/tcsh and
    /// fish login shells: no `!`, no newline, and backslashes only in the
    /// `'\''` sequences outside single quotes.
    #[test]
    fn remote_commands_are_login_shell_neutral() {
        let mut opts = BootstrapOptions::new(SshTarget::new("h"));
        opts.remote_state_dir = Some("~/my state".into());
        opts.replace = true;
        for script in [
            probe_script(&opts).unwrap(),
            upload_script(&opts).unwrap(),
            format!(
                "exec {} {}",
                bin_expr(&opts).unwrap(),
                opts.bootstrap_args().unwrap()
            ),
        ] {
            let cmd = format!("sh -c {}", sh_quote(&script));
            assert!(!cmd.contains('!'), "{cmd}");
            assert!(!cmd.contains('\n'), "{cmd}");
            assert_eq!(
                cmd.matches('\\').count(),
                cmd.matches("'\\''").count(),
                "stray backslash: {cmd}"
            );
        }
    }

    #[test]
    fn control_path_quoting_and_length() {
        let cp = control_path_option(Path::new("/Users/a b/100%/.civet/ssh")).unwrap();
        assert_eq!(cp, "ControlPath=\"/Users/a b/100%%/.civet/ssh/%C\"");
        let long = format!("/{}", "x".repeat(60));
        let err = control_path_option(Path::new(&long)).unwrap_err();
        assert!(err.contains("unix socket limit"), "{err}");
        assert!(control_path_option(Path::new("/a\"b")).is_err());
    }

    /// A real ssh accepts the quoted ControlPath (parsed with `-G`, no
    /// connection is made).
    #[test]
    fn control_path_parses_with_real_ssh() {
        let cp = control_path_option(Path::new("/tmp/a b/50%")).unwrap();
        let Ok(out) = std::process::Command::new("ssh")
            .args(["-G", "-o", &cp, "civet-test.invalid"])
            .output()
        else {
            return; // no ssh installed
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.starts_with("controlpath "))
            .unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(&out.stderr)));
        assert!(line.starts_with("controlpath /tmp/a b/50%/"), "{line}");
    }

    fn fake_elf(machine: u16, marker: Option<&str>) -> Vec<u8> {
        let mut d = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        d.resize(18, 0);
        d.extend_from_slice(&machine.to_le_bytes());
        d.resize(256, 0);
        if let Some(v) = marker {
            d.extend_from_slice(format!("\0CIVETD_VERSION={v}\0").as_bytes());
        }
        d.resize(512, 0);
        d
    }

    #[test]
    fn local_binary_check() {
        let p = Path::new("/d/civetd-x86_64-unknown-linux-musl");
        let t = "x86_64-unknown-linux-musl";
        assert!(check_binary(p, &fake_elf(62, Some(crate::VERSION)), t).is_ok());
        // Stale version: refused with the versions in the message.
        let e = check_binary(p, &fake_elf(62, Some("0.0.1")), t).unwrap_err();
        let msg = e.to_string();
        assert!(matches!(&e, BootstrapError::WrongBinary { got, .. } if got == "0.0.1"));
        assert!(msg.contains("build-remote.sh"), "{msg}");
        assert!(msg.contains(crate::VERSION), "{msg}");
        // No marker (built before markers existed).
        assert!(matches!(
            check_binary(p, &fake_elf(62, None), t),
            Err(BootstrapError::WrongBinary { .. })
        ));
        // Wrong architecture / format.
        assert!(check_binary(p, &fake_elf(183, Some(crate::VERSION)), t).is_err());
        assert!(
            check_binary(
                p,
                &fake_elf(183, Some(crate::VERSION)),
                "aarch64-unknown-linux-musl"
            )
            .is_ok()
        );
        let mut text = b"#!/bin/sh\n".to_vec();
        text.extend_from_slice(VERSION_MARKER.as_bytes());
        assert!(check_binary(p, &text, t).is_err());
        assert!(check_binary(p, &text, "aarch64-apple-darwin").is_err());
    }

    #[test]
    fn marker_roundtrip() {
        let bytes = version_marker_bytes();
        assert_eq!(&bytes[..], VERSION_MARKER.as_bytes());
        let mut data = vec![1u8; 100];
        data.extend_from_slice(&bytes);
        data.extend_from_slice(&[2u8; 100]);
        assert_eq!(binary_version(&data).as_deref(), Some(crate::VERSION));
        assert_eq!(binary_version(b"\0CIVETD_VERSION="), None);
    }

    /// Search order: the first candidate with the right version wins; a
    /// stale one is skipped; all stale → WrongBinary; none → NoBinary.
    #[tokio::test]
    async fn search_skips_stale_candidates() {
        let t = "x86_64-unknown-linux-musl";
        let dir = tempfile::tempdir().unwrap();
        let (stale, good, empty) = (
            dir.path().join("stale"),
            dir.path().join("good"),
            dir.path().join("empty"),
        );
        for d in [&stale, &good, &empty] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            stale.join(format!("civetd-{t}")),
            fake_elf(62, Some("0.0.1")),
        )
        .unwrap();
        std::fs::write(
            good.join(format!("civetd-{t}")),
            fake_elf(62, Some(crate::VERSION)),
        )
        .unwrap();
        let src = BinarySource::Search(vec![
            BinarySource::Dir(empty.clone()),
            BinarySource::Dir(stale.clone()),
            BinarySource::Dir(good.clone()),
        ]);
        let (path, _) = find_binary(&src, t).await.unwrap();
        assert_eq!(path, good.join(format!("civetd-{t}")));
        let src = BinarySource::Search(vec![
            BinarySource::Dir(empty.clone()),
            BinarySource::Dir(stale.clone()),
        ]);
        assert!(matches!(
            find_binary(&src, t).await,
            Err(BootstrapError::WrongBinary { .. })
        ));
        assert!(matches!(
            find_binary(&BinarySource::Dir(empty), t).await,
            Err(BootstrapError::NoBinary { .. })
        ));
        // A bundled dir comes first in the default order.
        let chain = BinarySource::with_bundled(Some(good.clone()));
        assert!(matches!(&chain, BinarySource::Search(v) if v[0] == BinarySource::Dir(good)));
    }

    /// The upload script refuses (exit 86, temp file removed) a binary
    /// whose `version` is not ours, and installs a good one.
    #[test]
    fn upload_script_checks_remote_version() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let mut opts = BootstrapOptions::new(SshTarget::new("h"));
        opts.remote_state_dir = Some("~/st".into());
        let script = upload_script(&opts).unwrap();
        let run = |body: &str| {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .env("HOME", home.path())
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            SshOutput {
                code: out.status.code(),
                stdout: String::new(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }
        };
        let bin = home.path().join("st/bin");
        let out = run("#!/bin/sh\necho 'civetd 0.0.1 (proto 1, x)'\n");
        assert_eq!(out.code, Some(EXIT_UPLOAD_REJECTED), "{}", out.stderr);
        assert!(matches!(
            upload_error(&out),
            BootstrapError::UploadRejected { got, .. } if got.starts_with("civetd 0.0.1")
        ));
        assert_eq!(
            std::fs::read_dir(&bin).unwrap().count(),
            0,
            "temp file left"
        );
        // Truncated / not executable at all.
        let out = run("\x7fELF");
        assert_eq!(out.code, Some(EXIT_UPLOAD_REJECTED), "{}", out.stderr);
        let good = format!("#!/bin/sh\necho 'civetd {} (proto 1, x)'\n", crate::VERSION);
        let out = run(&good);
        assert_eq!(out.code, Some(0), "{}", out.stderr);
        assert!(bin.join(format!("civetd-{}", crate::VERSION)).is_file());
        assert!(bin.join("civetd").exists());
    }
}
