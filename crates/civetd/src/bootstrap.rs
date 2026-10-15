//! Client side of bootstrapping (available with `default-features = false`).
//!
//! Like mosh: use the system `ssh` (so `~/.ssh/config`, agents, ProxyJump
//! and ControlMaster all behave as the user expects) to
//!
//! 1. **probe** the remote (`uname`, `$HOME`, whether
//!    `~/.civet/bin/civetd-<our version>` exists; if it does, run its
//!    `bootstrap` in the same round trip),
//! 2. **upload** the matching static binary if missing (version-suffixed, so
//!    a running daemon's file is never overwritten),
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
//! zsh, fish…) only ever sees one quoted word.
//!
//! # Building the binaries to upload
//!
//! The client looks for `civetd-<target-triple>` in a directory
//! ([`BinarySource::Dir`], default `$CIVET_DIST_DIR` or `~/.civet/dist`).
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
}

impl BinarySource {
    /// `$CIVET_DIST_DIR` if set, else `~/.civet/dist` if it exists, else
    /// [`BinarySource::CurrentExeSibling`].
    pub fn from_env() -> Self {
        if let Some(d) = std::env::var_os("CIVET_DIST_DIR").filter(|d| !d.is_empty()) {
            return Self::Dir(PathBuf::from(d));
        }
        let dist = StatePaths::new(default_state_dir()).dist_dir();
        if dist.is_dir() {
            return Self::Dir(dist);
        }
        Self::CurrentExeSibling
    }
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

/// Shell expression for a remote path; a leading `~/` becomes `"$HOME"/`.
fn remote_path_expr(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sh_quote(rest))
    } else {
        sh_quote(path)
    }
}

impl BootstrapOptions {
    /// Shell expression for the remote state dir.
    fn state_expr(&self) -> String {
        remote_path_expr(self.remote_state_dir.as_deref().unwrap_or("~/.civet"))
    }

    /// `civetd bootstrap` arguments (already quoted).
    fn bootstrap_args(&self) -> String {
        let mut s = String::from("bootstrap");
        if let Some(d) = &self.remote_state_dir {
            s.push_str(" --state-dir ");
            s.push_str(&remote_path_expr(d));
        }
        if self.replace {
            s.push_str(" --replace");
        }
        s
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
            for opt in [
                "BatchMode=yes".to_string(),
                "ControlMaster=auto".to_string(),
                format!("ControlPath={}/%C", ssh_dir.display()),
                "ControlPersist=60".to_string(),
            ] {
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
        }
    }
    r
}

/// Remote binary path expression (shell) for our version.
fn bin_expr(opts: &BootstrapOptions) -> String {
    format!("{}/bin/civetd-{}", opts.state_expr(), crate::VERSION)
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
    let probe_script = format!(
        "echo \"CIVET_UNAME=$(uname -s) $(uname -m)\"; echo \"CIVET_HOME=$HOME\"; \
         B={bin}; if [ -x \"$B\" ]; then echo CIVET_HAVE=1; \"$B\" {args}; echo \"CIVET_EXIT=$?\"; \
         else echo CIVET_HAVE=0; fi",
        bin = bin_expr(opts),
        args = opts.bootstrap_args(),
    );
    let out = run_ssh(opts, &probe_script, None, "ssh probe").await?;
    let probe = parse_probe(&out.stdout);
    let Some((uname_s, uname_m)) = probe.uname.clone() else {
        return Err(BootstrapError::Ssh {
            code: out.code,
            stderr: hint(&out.stderr),
        });
    };
    let home = probe.home.clone().unwrap_or_default();
    let remote_binary = absolute_remote_binary(opts, &home);

    let mut installed = false;
    let info = match (probe.have_binary, probe.bootstrap_exit) {
        // 126/127: present but not runnable (wrong arch, corrupt): reinstall.
        (true, Some(code)) if code != 126 && code != 127 => {
            bootstrap_result(Some(code), &out.stdout, &out.stderr)?
        }
        _ => {
            // 2. Upload.
            progress(BootstrapStep::Upload);
            let triple = target_triple(&uname_s, &uname_m)?;
            let local = find_binary(&opts.binary, &triple)?;
            let data = tokio::fs::read(&local).await?;
            let upload_script = format!(
                "set -e; umask 077; S={state}; mkdir -p \"$S/bin\"; \
                 T=\"$S/bin/.civetd-{ver}.tmp.$$\"; cat > \"$T\"; chmod 755 \"$T\"; \
                 mv -f \"$T\" \"$S/bin/civetd-{ver}\"; ln -sf civetd-{ver} \"$S/bin/civetd\"",
                state = opts.state_expr(),
                ver = crate::VERSION,
            );
            let up = run_ssh(opts, &upload_script, Some(data), "ssh upload").await?;
            if up.code != Some(0) {
                return Err(BootstrapError::Ssh {
                    code: up.code,
                    stderr: hint(&up.stderr),
                });
            }
            installed = true;
            // 3. Start.
            progress(BootstrapStep::Start);
            let start_script = format!("exec {} {}", bin_expr(opts), opts.bootstrap_args());
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

/// Locate the binary to upload for `triple`.
fn find_binary(source: &BinarySource, triple: &str) -> Result<PathBuf, BootstrapError> {
    let no = |searched: String| BootstrapError::NoBinary {
        triple: triple.to_string(),
        searched,
    };
    match source {
        BinarySource::Dir(dir) => {
            let p = dir.join(format!("civetd-{triple}"));
            if p.is_file() {
                Ok(p)
            } else {
                Err(no(p.display().to_string()))
            }
        }
        BinarySource::File(p) => {
            if p.is_file() {
                Ok(p.clone())
            } else {
                Err(no(p.display().to_string()))
            }
        }
        BinarySource::CurrentExeSibling => {
            let p = sibling_civetd()?;
            if !same_platform(crate::TARGET, triple) {
                return Err(no(format!(
                    "{} (built for {}, set CIVET_DIST_DIR for other targets)",
                    p.display(),
                    crate::TARGET
                )));
            }
            if p.is_file() {
                Ok(p)
            } else {
                Err(no(p.display().to_string()))
            }
        }
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

/// Run `<civetd> bootstrap [--state-dir]` as a local child (no ssh); dial
/// address 127.0.0.1. Used by `civet-cli --local` and desktop "Local".
pub async fn bootstrap_local(
    civetd: &Path,
    state_dir: Option<&Path>,
) -> Result<Bootstrapped, BootstrapError> {
    let mut cmd = Command::new(civetd);
    cmd.arg("bootstrap");
    if let Some(d) = state_dir {
        cmd.arg("--state-dir").arg(d);
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
    let mut script = format!("exec {} stdio-tunnel", remote_path_expr(remote_binary));
    if let Some(d) = &opts.remote_state_dir {
        script.push_str(" --state-dir ");
        script.push_str(&remote_path_expr(d));
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
        assert_eq!(remote_path_expr("~/.civet"), "\"$HOME\"/.civet");
        assert_eq!(remote_path_expr("~/my dir"), "\"$HOME\"/'my dir'");
        assert_eq!(remote_path_expr("/tmp/x;y"), "'/tmp/x;y'");
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
}
