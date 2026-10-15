//! The app's [`Connector`]: bootstrap over SSH (or a local `civetd`), then
//! QUIC with a 3 s connect timeout, falling back to the stdio tunnel when
//! UDP is blocked or when forced (docs/INTERFACES.md §6.5). With `Auto` the
//! session uses a [`FailoverDialer`], so it also fails over when UDP gets
//! blocked mid-session and moves back to QUIC once it works again.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use civet_proto::{CertFingerprint, ClientId, SessionKey};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, FailoverConfig, FailoverDialer, LinkState,
    QuicClient, QuicClientConfig, QuicDialer, StreamDialer, TransportKind,
};
use civetd::bootstrap::{
    BUNDLED_DIST_DIR, BinarySource, BootstrapError, BootstrapOptions, BootstrapStep, Bootstrapped,
    SshTarget, bootstrap_local_with, bootstrap_ssh, check_binary, open_local_tunnel,
    open_ssh_tunnel, sibling_civetd,
};
use futures::future::BoxFuture;

use crate::error::CmdError;
use crate::events::{DaemonUpgrade, HostTarget, ProgressStep, TransportChoice};
use crate::hosts::{Connector, Established, Progress};

/// QUIC connect timeout before falling back to the tunnel.
pub const QUIC_DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Client name in the hello.
pub const CLIENT_NAME: &str = "civet-desktop";

/// Bootstraps with `civetd::bootstrap` and dials.
#[derive(Debug, Clone)]
pub struct SshConnector {
    /// Stable per installation (`~/.civet/client-id-desktop`).
    pub client_id: ClientId,
    /// `civetd` for the local host; None = search (see [`find_local_civetd`]).
    pub local_civetd: Option<PathBuf>,
    /// State dir of the local daemon; None = its default (`~/.civet`).
    pub local_state_dir: Option<PathBuf>,
    /// The app's bundled `civet-dist` resource dir (`civetd-<triple>` for
    /// Linux remotes and this Mac), searched first; None in tests / when the
    /// app has no resources (dev builds fall back to `$CIVET_DIST_DIR`,
    /// `~/.civet/dist` and the workspace `civetd`).
    pub bundled_dist: Option<PathBuf>,
}

impl SshConnector {
    /// A connector for the app: `bundled_dist` = `<resources>/civet-dist`.
    pub fn new(client_id: ClientId, resource_dir: Option<&Path>) -> Self {
        Self {
            client_id,
            local_civetd: None,
            local_state_dir: None,
            bundled_dist: resource_dir.map(|d| d.join(BUNDLED_DIST_DIR)),
        }
    }
}

fn step(s: BootstrapStep) -> ProgressStep {
    match s {
        BootstrapStep::Probe => ProgressStep::Probe,
        BootstrapStep::Upload => ProgressStep::Upload,
        BootstrapStep::Start => ProgressStep::Start,
        BootstrapStep::Resolve => ProgressStep::Resolve,
    }
}

/// `daemonBusy` for an incompatible daemon with live panes (the UI offers
/// "Restart daemon" = `restart_daemon`), else `bootstrap`.
fn bootstrap_err(e: BootstrapError) -> CmdError {
    match e {
        BootstrapError::IncompatibleDaemonBusy => CmdError::new(
            "daemonBusy",
            "the running civetd is incompatible with this app and has live panes; \
             restart the daemon to upgrade it (closes its panes)",
        ),
        other => CmdError::bootstrap(other.to_string()),
    }
}

fn io_err(e: BootstrapError) -> std::io::Error {
    match e {
        BootstrapError::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

/// The bundled `civetd-<this triple>` in `dist`, if it is a binary of this
/// app's version for this platform.
pub fn bundled_local_civetd(dist: &Path) -> Option<PathBuf> {
    let p = dist.join(format!("civetd-{}", civetd::TARGET));
    let data = std::fs::read(&p).ok()?;
    match check_binary(&p, &data, civetd::TARGET) {
        Ok(()) => Some(p),
        Err(e) => {
            tracing::warn!("skipping bundled {}: {e}", p.display());
            None
        }
    }
}

/// Locate `civetd` for the local host: `$CIVET_CIVETD`, the app bundle's
/// `civet-dist/civetd-<triple>` (packaged app), next to this executable
/// (dev builds: the workspace `target/<profile>/civetd`), on `$PATH`, or
/// `~/.civet/bin/civetd`.
pub fn find_local_civetd(bundled_dist: Option<&Path>) -> Option<PathBuf> {
    let is_file = |p: &Path| p.is_file();
    if let Some(p) = std::env::var_os("CIVET_CIVETD").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = bundled_dist.and_then(bundled_local_civetd) {
        return Some(p);
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

/// Dial according to `choice`: `Quic` = QUIC only, `Ssh` = the tunnel only,
/// `Auto` = a [`FailoverDialer`] (QUIC first; the tunnel when UDP is blocked
/// at connect time or later; back to QUIC when it works again).
pub async fn dial(
    config: ClientSessionConfig,
    choice: TransportChoice,
    quic: Option<(std::net::SocketAddr, CertFingerprint)>,
    tunnel: Arc<dyn Dialer>,
    daemon_version: String,
    upgrade: Option<DaemonUpgrade>,
    progress: &Progress,
) -> Result<Established, CmdError> {
    let quic = match quic {
        Some((addr, fingerprint)) if choice != TransportChoice::Ssh => {
            progress(ProgressStep::Dial, Some(format!("QUIC {addr}")));
            let client = Arc::new(QuicClient::new(addr.is_ipv6())?);
            let mut cfg = QuicClientConfig::new(addr, fingerprint);
            cfg.connect_timeout = QUIC_DIAL_TIMEOUT;
            let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
                client,
                config: cfg,
            });
            Some((addr, dialer))
        }
        _ => None,
    };
    let (dialer, what): (Arc<dyn Dialer>, String) = match (choice, quic) {
        (TransportChoice::Quic, Some((addr, q))) => (q, format!("QUIC to {addr}")),
        (TransportChoice::Auto, Some((addr, q))) => {
            let failover = FailoverConfig {
                primary_timeout: QUIC_DIAL_TIMEOUT,
                ..FailoverConfig::default()
            };
            (
                Arc::new(FailoverDialer::new(q, tunnel, failover)),
                format!("QUIC to {addr} (or the stdio tunnel)"),
            )
        }
        _ => (tunnel, "stdio tunnel".to_string()),
    };
    let (session, events) = ClientSession::connect(config, dialer)
        .await
        .map_err(|e| CmdError::transport(format!("{what}: {e}")))?;
    let transport = match &*session.link_state().borrow() {
        LinkState::Connected { transport, .. } => *transport,
        _ => TransportKind::Stream,
    };
    if choice == TransportChoice::Auto && transport == TransportKind::Stream {
        tracing::info!("QUIC unreachable; connected over the stdio tunnel");
        progress(
            ProgressStep::Fallback,
            Some("UDP unreachable; using the SSH tunnel".into()),
        );
    }
    Ok(Established {
        session,
        events,
        transport,
        daemon_version,
        // The failover dialer handles fallback and recovery itself, so the
        // choice is kept for re-bootstraps.
        choice,
        upgrade,
    })
}

impl SshConnector {
    async fn connect_ssh(
        &self,
        target: SshTarget,
        choice: TransportChoice,
        replace: bool,
        progress: Progress,
    ) -> Result<Established, CmdError> {
        let mut opts = BootstrapOptions::new(target);
        opts.binary = BinarySource::with_bundled(self.bundled_dist.clone());
        opts.replace = replace;
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
            DaemonUpgrade::from_bootstrap(&b.info),
            &progress,
        )
        .await
    }

    async fn connect_local(
        &self,
        choice: TransportChoice,
        replace: bool,
        progress: Progress,
    ) -> Result<Established, CmdError> {
        let civetd = self
            .local_civetd
            .clone()
            .or_else(|| find_local_civetd(self.bundled_dist.as_deref()))
            .ok_or_else(|| {
                CmdError::bootstrap(
                    "civetd not found: set CIVET_CIVETD, put civetd next to the app or on PATH",
                )
            })?;
        progress(ProgressStep::Start, Some(civetd.display().to_string()));
        let state_dir = self.local_state_dir.clone();
        let b = bootstrap_local_with(&civetd, state_dir.as_deref(), replace)
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
            DaemonUpgrade::from_bootstrap(&b.info),
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
        replace: bool,
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
                    self.connect_ssh(t, choice, replace, progress).await
                }
                HostTarget::Local => self.connect_local(choice, replace, progress).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_daemon_maps_to_daemon_busy() {
        assert_eq!(
            bootstrap_err(BootstrapError::IncompatibleDaemonBusy).code,
            "daemonBusy"
        );
        assert_eq!(
            bootstrap_err(BootstrapError::Timeout("ssh probe")).code,
            "bootstrap"
        );
    }

    #[test]
    fn bundled_local_civetd_requires_a_matching_binary() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(bundled_local_civetd(dir.path()), None, "missing");
        let p = dir.path().join(format!("civetd-{}", civetd::TARGET));
        std::fs::write(&p, b"not a binary").unwrap();
        assert_eq!(bundled_local_civetd(dir.path()), None, "wrong file");
        let c = SshConnector::new(ClientId::generate().unwrap(), Some(Path::new("/res")));
        assert_eq!(c.bundled_dist, Some(PathBuf::from("/res/civet-dist")));
    }
}
