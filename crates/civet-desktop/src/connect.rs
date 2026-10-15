//! The app's [`Connector`]: bootstrap over SSH (or a local `civetd`), then
//! QUIC with a 3 s connect timeout, falling back to the stdio tunnel when
//! UDP is blocked or when forced (docs/INTERFACES.md §6.5).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use civet_proto::{CertFingerprint, ClientId, SessionKey};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, QuicClient, QuicClientConfig, QuicDialer,
    StreamDialer, TransportKind,
};
use civetd::bootstrap::{
    BootstrapError, BootstrapOptions, BootstrapStep, Bootstrapped, SshTarget, bootstrap_local,
    bootstrap_ssh, open_local_tunnel, open_ssh_tunnel, sibling_civetd,
};
use futures::future::BoxFuture;

use crate::error::CmdError;
use crate::events::{HostTarget, ProgressStep, TransportChoice};
use crate::hosts::{Connector, Established, Progress};

/// QUIC connect timeout before falling back to the tunnel.
pub const QUIC_DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Client name in the hello.
pub const CLIENT_NAME: &str = "civet-desktop";

/// Bootstraps with `civetd::bootstrap` and dials.
#[derive(Debug, Clone)]
pub struct SshConnector {
    /// Stable per installation (`~/.civet/client-id`).
    pub client_id: ClientId,
    /// `civetd` for the local host; None = search (see [`find_local_civetd`]).
    pub local_civetd: Option<PathBuf>,
    /// State dir of the local daemon; None = its default (`~/.civet`).
    pub local_state_dir: Option<PathBuf>,
}

fn step(s: BootstrapStep) -> ProgressStep {
    match s {
        BootstrapStep::Probe => ProgressStep::Probe,
        BootstrapStep::Upload => ProgressStep::Upload,
        BootstrapStep::Start => ProgressStep::Start,
        BootstrapStep::Resolve => ProgressStep::Resolve,
    }
}

fn bootstrap_err(e: BootstrapError) -> CmdError {
    CmdError::bootstrap(e.to_string())
}

fn io_err(e: BootstrapError) -> std::io::Error {
    match e {
        BootstrapError::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

/// Locate `civetd` for the local host: `$CIVET_CIVETD`, next to this
/// executable (dev builds, bundled sidecar), on `$PATH`, or
/// `~/.civet/bin/civetd`.
pub fn find_local_civetd() -> Option<PathBuf> {
    let is_file = |p: &Path| p.is_file();
    if let Some(p) = std::env::var_os("CIVET_CIVETD").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Ok(p) = sibling_civetd()
        && is_file(&p)
    {
        return Some(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("civetd");
            if is_file(&p) {
                return Some(p);
            }
        }
    }
    let p = civetd::paths::StatePaths::new(civetd::paths::default_state_dir())
        .bin_dir()
        .join("civetd");
    is_file(&p).then_some(p)
}

/// Session config for this app.
pub fn session_config(key: SessionKey, client_id: ClientId) -> ClientSessionConfig {
    ClientSessionConfig::new(key, client_id, CLIENT_NAME, env!("CARGO_PKG_VERSION"))
}

/// Dial QUIC (unless `choice` is `Ssh`), falling back to `tunnel` on a
/// non-fatal failure when `choice` is `Auto`.
pub async fn dial(
    config: ClientSessionConfig,
    choice: TransportChoice,
    quic: Option<(std::net::SocketAddr, CertFingerprint)>,
    tunnel: Arc<dyn Dialer>,
    daemon_version: String,
    progress: &Progress,
) -> Result<Established, CmdError> {
    if choice != TransportChoice::Ssh
        && let Some((addr, fingerprint)) = quic
    {
        progress(ProgressStep::Dial, Some(format!("QUIC {addr}")));
        let client = Arc::new(QuicClient::new(addr.is_ipv6())?);
        let mut cfg = QuicClientConfig::new(addr, fingerprint);
        cfg.connect_timeout = QUIC_DIAL_TIMEOUT;
        let dialer = Arc::new(QuicDialer {
            client,
            config: cfg,
        });
        match ClientSession::connect(config.clone(), dialer).await {
            Ok((session, events)) => {
                return Ok(Established {
                    session,
                    events,
                    transport: TransportKind::Quic,
                    daemon_version,
                    choice,
                });
            }
            Err(e) if choice == TransportChoice::Quic || e.is_fatal() => {
                return Err(CmdError::transport(format!("QUIC to {addr}: {e}")));
            }
            Err(e) => {
                tracing::info!(%addr, error = %e, "QUIC failed; falling back to the stdio tunnel");
                progress(
                    ProgressStep::Fallback,
                    Some(format!("UDP unreachable ({e}); using the SSH tunnel")),
                );
            }
        }
    }
    let (session, events) = ClientSession::connect(config, tunnel)
        .await
        .map_err(|e| CmdError::transport(format!("stdio tunnel: {e}")))?;
    Ok(Established {
        session,
        events,
        transport: TransportKind::Stream,
        daemon_version,
        // Remember the fallback for the rest of the app session.
        choice: TransportChoice::Ssh,
    })
}

impl SshConnector {
    async fn connect_ssh(
        &self,
        target: SshTarget,
        choice: TransportChoice,
        progress: Progress,
    ) -> Result<Established, CmdError> {
        let opts = BootstrapOptions::new(target);
        let p = progress.clone();
        let b: Bootstrapped = bootstrap_ssh(&opts, &move |s| p(step(s), None))
            .await
            .map_err(bootstrap_err)?;
        let remote_binary = b.remote_binary.clone();
        let tunnel = Arc::new(StreamDialer {
            factory: move || {
                let opts = opts.clone();
                let remote_binary = remote_binary.clone();
                async move { open_ssh_tunnel(&opts, &remote_binary).await.map_err(io_err) }
            },
        });
        dial(
            session_config(b.info.session_key.clone(), self.client_id),
            choice,
            Some((b.dial_addr, b.info.cert_fingerprint)),
            tunnel,
            b.info.daemon_version.clone(),
            &progress,
        )
        .await
    }

    async fn connect_local(
        &self,
        choice: TransportChoice,
        progress: Progress,
    ) -> Result<Established, CmdError> {
        let civetd = self
            .local_civetd
            .clone()
            .or_else(find_local_civetd)
            .ok_or_else(|| {
                CmdError::bootstrap(
                    "civetd not found: set CIVET_CIVETD, put civetd next to the app or on PATH",
                )
            })?;
        progress(ProgressStep::Start, Some(civetd.display().to_string()));
        let state_dir = self.local_state_dir.clone();
        let b = bootstrap_local(&civetd, state_dir.as_deref())
            .await
            .map_err(bootstrap_err)?;
        let tunnel = Arc::new(StreamDialer {
            factory: move || {
                let civetd = civetd.clone();
                let state_dir = state_dir.clone();
                async move {
                    open_local_tunnel(&civetd, state_dir.as_deref())
                        .await
                        .map_err(io_err)
                }
            },
        });
        dial(
            session_config(b.info.session_key.clone(), self.client_id),
            choice,
            Some((b.dial_addr, b.info.cert_fingerprint)),
            tunnel,
            b.info.daemon_version.clone(),
            &progress,
        )
        .await
    }
}

impl Connector for SshConnector {
    fn connect<'a>(
        &'a self,
        target: &'a HostTarget,
        choice: TransportChoice,
        progress: Progress,
    ) -> BoxFuture<'a, Result<Established, CmdError>> {
        Box::pin(async move {
            match target {
                HostTarget::Ssh {
                    destination,
                    port,
                    extra_args,
                } => {
                    let t = SshTarget {
                        destination: destination.trim().to_string(),
                        port: *port,
                        extra_args: extra_args.clone(),
                    };
                    self.connect_ssh(t, choice, progress).await
                }
                HostTarget::Local => self.connect_local(choice, progress).await,
            }
        })
    }
}
