//! The daemon: `serve`, the server side of `bootstrap`, and `stdio-tunnel`.
//!
//! # Lifecycle
//!
//! `serve` takes an exclusive `flock` on `<state>/civetd.lock` (single
//! instance per user and state dir), generates a fresh session key, loads
//! (or creates) the persistent TLS identity, binds QUIC in the port range and
//! the unix socket `<state>/civetd.sock`, and writes `<state>/daemon.json`
//! **last**, so its existence plus a held lock means "ready". The daemon does
//! not care whether any SSH session is still around: clients reach it over
//! QUIC (or a fresh `ssh … civetd stdio-tunnel`), and panes live until they
//! are killed or the daemon stops. On SIGTERM / SIGINT it sends
//! `ServerMessage::Goodbye` to every client, closes their connections with
//! code 4, kills all panes and removes `daemon.json` and the socket. SIGHUP
//! is ignored.
//!
//! # Upgrades while sessions run
//!
//! Binaries are installed side by side (`bin/civetd-<version>`), so the
//! running daemon's file is never touched. When `civetd bootstrap` (new
//! version) finds a running daemon:
//!
//! * **Compatible protocol, has panes** → reattach: print the running
//!   daemon's line (`reattached: true`, its own `daemon_version`). Sessions
//!   keep running on the old daemon; nothing is interrupted.
//! * **Compatible protocol, no panes, older version** → upgrade: SIGTERM the
//!   old daemon (connected clients get `Goodbye` and re-bootstrap), wait up
//!   to 5 s for it to release the lock, start the new one. Nothing is lost
//!   because nothing was running.
//! * **Incompatible protocol** (or `--replace`): if it has no panes (or
//!   `--replace` was given) it is stopped the same way and replaced;
//!   otherwise bootstrap exits 4 and says so on stderr (the client reports
//!   `IncompatibleDaemonBusy` and may retry with `replace`).
//!
//! A newer running daemon is never downgraded unless `--replace` is given.

mod bootstrap;
mod ports;
mod server;
mod sys;
mod uploads;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use civet_proto::{BootstrapInfo, Event, SessionKey};
use civet_session::{SessionConfig, SessionManager};
use civet_transport::{QuicServer, ServerIdentity};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

use crate::bootstrap::exit_code;
use crate::paths::{StatePaths, create_private_dir, set_mode, write_atomic};

pub use bootstrap::bootstrap;
pub use server::RESUME_TTL;
pub use sys::daemonize;

/// Error that carries a process exit code (see INTERFACES.md 5.1).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ExitError {
    /// Process exit code.
    pub code: i32,
    /// Message for stderr.
    pub message: String,
}

impl ExitError {
    /// Construct.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Options for [`serve`] / [`Daemon::start`].
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    /// State directory (`~/.civet`).
    pub state_dir: PathBuf,
    /// QUIC bind address (default 0.0.0.0).
    pub bind_ip: IpAddr,
    /// QUIC ports to try in order (default 60001-60999; `0..=0` = any).
    pub port_range: RangeInclusive<u16>,
    /// Shell for panes (None = `$SHELL`, else /bin/bash, else /bin/sh).
    pub shell: Option<String>,
}

impl DaemonOptions {
    /// Defaults for `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            bind_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port_range: civet_transport::DEFAULT_PORT_RANGE,
            shell: None,
        }
    }
}

/// Parse `"60001-60999"` (or a single port).
pub fn parse_port_range(s: &str) -> anyhow::Result<RangeInclusive<u16>> {
    let s = s.trim();
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let a: u16 = a.trim().parse().context("bad port range start")?;
    let b: u16 = b.trim().parse().context("bad port range end")?;
    anyhow::ensure!(a <= b, "empty port range {s}");
    Ok(a..=b)
}

/// A running daemon (in this process).
#[derive(Debug)]
pub struct Daemon {
    server: Arc<server::Server>,
    info: BootstrapInfo,
    paths: StatePaths,
    quic: Arc<QuicServer>,
    tasks: Vec<JoinHandle<()>>,
    _lock: sys::LockFile,
}

impl Daemon {
    /// Take the lock, bind everything, write `daemon.json`. Fails with an
    /// [`ExitError`] (code 3) if another daemon holds the lock.
    pub async fn start(opts: DaemonOptions) -> anyhow::Result<Daemon> {
        let paths = StatePaths::new(&opts.state_dir);
        create_private_dir(&paths.root)
            .with_context(|| format!("creating {}", paths.root.display()))?;
        let lock = match sys::try_lock(&paths.lock())? {
            sys::LockAttempt::Acquired(l) => l,
            sys::LockAttempt::Held => {
                return Err(ExitError::new(
                    exit_code::ALREADY_RUNNING,
                    format!(
                        "civetd is already running for {} (lock held)",
                        paths.root.display()
                    ),
                )
                .into());
            }
        };
        // We hold the lock: anything left over is stale.
        let _ = std::fs::remove_file(paths.daemon_json());
        let _ = std::fs::remove_file(paths.socket());

        let key = SessionKey::generate()?;
        let instance_id = random_u64()?;
        let identity = ServerIdentity::load_or_generate(&paths.root)?;
        let quic = Arc::new(
            QuicServer::bind_port_range(
                opts.bind_ip,
                opts.port_range.clone(),
                identity,
                key.clone(),
            )
            .with_context(|| format!("binding QUIC in {:?}", opts.port_range))?,
        );

        let mut session_config = SessionConfig::new(&paths.root);
        session_config.shell = opts.shell.clone();
        let sessions = SessionManager::new(session_config)?;
        let server = Arc::new(server::Server::new(
            sessions,
            key.clone(),
            instance_id,
            sys::hostname(),
        ));

        let listener = UnixListener::bind(paths.socket())
            .with_context(|| format!("binding {}", paths.socket().display()))?;
        set_mode(&paths.socket(), 0o600)?;

        let tasks = vec![
            tokio::spawn(accept_quic(server.clone(), quic.clone())),
            tokio::spawn(accept_unix(server.clone(), listener)),
            tokio::spawn(housekeeping(server.clone())),
        ];

        let info = BootstrapInfo::new(
            crate::VERSION,
            quic.local_addr().port(),
            key,
            quic.fingerprint(),
        );
        write_atomic(&paths.daemon_json(), info.to_json_line().as_bytes(), 0o600)?;
        tracing::info!(
            version = crate::VERSION,
            udp = %quic.local_addr(),
            socket = %paths.socket().display(),
            pid = info.pid,
            "civetd ready"
        );
        Ok(Daemon {
            server,
            info,
            paths,
            quic,
            tasks,
            _lock: lock,
        })
    }

    /// The bootstrap line this daemon advertises (`reattached: false`).
    pub fn info(&self) -> &BootstrapInfo {
        &self.info
    }

    /// The unix socket path.
    pub fn socket_path(&self) -> PathBuf {
        self.paths.socket()
    }

    /// The bound QUIC address.
    pub fn quic_addr(&self) -> SocketAddr {
        self.quic.local_addr()
    }

    /// The session manager (tests, diagnostics).
    pub fn sessions(&self) -> &SessionManager {
        &self.server.sessions
    }

    /// Graceful shutdown: Goodbye to every client, close connections (code
    /// 4), kill panes, remove `daemon.json` and the socket, release the lock.
    pub async fn shutdown(self, reason: &str) {
        tracing::info!(reason, "shutting down");
        for t in &self.tasks {
            t.abort();
        }
        self.server.goodbye_all(reason).await;
        self.quic.close(reason);
        self.server.sessions.shutdown().await;
        // Only remove daemon.json if it is still ours.
        if let Ok(s) = std::fs::read_to_string(self.paths.daemon_json())
            && BootstrapInfo::from_json_line(&s).is_ok_and(|i| i.pid == self.info.pid)
        {
            let _ = std::fs::remove_file(self.paths.daemon_json());
        }
        let _ = std::fs::remove_file(self.paths.socket());
        // The lock is released when `self` drops here.
    }
}

fn random_u64() -> anyhow::Result<u64> {
    let bytes = SessionKey::generate()?.0;
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[..8]);
    Ok(u64::from_le_bytes(b))
}

/// At most this many connections may be in their (unauthenticated) TLS
/// handshake + hello at once; more are refused until one finishes.
pub const MAX_PENDING_HANDSHAKES: usize = 32;

fn handshake_slots() -> Arc<tokio::sync::Semaphore> {
    Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_HANDSHAKES))
}

async fn accept_quic(server: Arc<server::Server>, quic: Arc<QuicServer>) {
    let slots = handshake_slots();
    while let Some(incoming) = quic.accept().await {
        let remote = incoming.remote_addr();
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            // Dropping the incoming connection refuses it.
            tracing::debug!(%remote, "too many pending handshakes; refusing");
            continue;
        };
        let server = server.clone();
        tokio::spawn(async move {
            let r = incoming.handshake().await;
            drop(permit);
            match r {
                Ok(conn) => server.handle_connection(conn).await,
                Err(e) => tracing::info!(%remote, error = %e, "QUIC handshake failed"),
            }
        });
    }
}

async fn accept_unix(server: Arc<server::Server>, listener: UnixListener) {
    let slots = handshake_slots();
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    tracing::debug!("too many pending unix socket hellos; refusing");
                    continue;
                };
                let server = server.clone();
                tokio::spawn(async move {
                    let r = civet_transport::accept_stream(stream, &server.key).await;
                    drop(permit);
                    match r {
                        Ok(conn) => server.handle_connection(conn).await,
                        Err(e) => tracing::info!(error = %e, "unix socket hello failed"),
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "unix accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Prune subscriptions of closed panes; expire old resume state.
async fn housekeeping(server: Arc<server::Server>) {
    let mut events = server.sessions.subscribe_events();
    let mut reap = tokio::time::interval(Duration::from_secs(60));
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(Event::PaneClosed { pane }) => server.pane_closed(pane),
                Ok(_) => {}
                // Missed PaneClosed events: prune against the live panes.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    server.prune_closed_panes();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            _ = reap.tick() => {
                server.reap_expired();
            }
        }
    }
}

/// `civetd serve` (after optional daemonizing, which `main` does before the
/// runtime starts): run until SIGTERM / SIGINT, then shut down gracefully.
pub async fn serve(opts: DaemonOptions) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let daemon = Daemon::start(opts).await?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = hup.recv() => tracing::info!("ignoring SIGHUP"),
        }
    }
    daemon.shutdown("daemon shutting down").await;
    Ok(())
}

/// `civetd stdio-tunnel`: copy bytes between stdin/stdout and the daemon's
/// unix socket until either side closes. [`ExitError`] code 2 if no daemon
/// is listening.
pub async fn stdio_tunnel(state_dir: &Path) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let socket = StatePaths::new(state_dir).socket();
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .map_err(|e| {
            ExitError::new(
                exit_code::NO_DAEMON,
                format!("no civetd running ({}: {e})", socket.display()),
            )
        })?;
    let (mut sock_r, mut sock_w) = stream.into_split();
    let up = async {
        let mut stdin = tokio::io::stdin();
        let r = tokio::io::copy(&mut stdin, &mut sock_w).await;
        let _ = sock_w.shutdown().await;
        r
    };
    let down = async {
        let mut stdout = tokio::io::stdout();
        let r = tokio::io::copy(&mut sock_r, &mut stdout).await;
        let _ = stdout.flush().await;
        r
    };
    tokio::select! {
        r = up => { r.context("stdin -> daemon")?; }
        r = down => { r.context("daemon -> stdout")?; }
    }
    Ok(())
}
