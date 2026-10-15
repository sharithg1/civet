//! Port detection and forwarding for one host.
//!
//! * [`watch_ports`]: per session, keeps the host's listening-port list
//!   current from the daemon's `StreamOpen::Ports` stream and reports it as
//!   `civet://ports`.
//! * [`Forwards`]: per host, a local TCP listener per forwarded port; every
//!   accepted connection becomes one `StreamOpen::Forward` QUIC stream on
//!   whatever session the host has at that moment, so forwards survive
//!   reconnects and re-bootstraps (only connections open at the time of a
//!   connection loss break; browsers retry them).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use civet_proto::{ErrorCode, PortsUpdate};
use civet_transport::{ClientSession, LinkState, RequestError};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::task::{JoinHandle, JoinSet};

use crate::error::CmdError;
use crate::events::{ForwardView, ForwardsEvent, PortsEvent, PortsStatus, UiEvents};

/// How long a new local connection waits for the host to be reachable
/// (e.g. right after wake) before it is closed.
const CONNECT_PATIENCE: Duration = Duration::from_secs(15);
/// Pause between attempts while waiting for the host.
const RETRY_PAUSE: Duration = Duration::from_millis(500);
/// Pause before re-opening a ports watch that ended with the session alive.
const REWATCH_PAUSE: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The host's current session (None while it has none).
pub type SessionGetter = Arc<dyn Fn() -> Option<ClientSession> + Send + Sync>;

// ---------------------------------------------------------------------------
// Port detection
// ---------------------------------------------------------------------------

/// The last ports report of a host (for `list_ports` after a UI reload).
#[derive(Debug)]
pub struct PortsState {
    host_id: String,
    current: Mutex<PortsEvent>,
}

impl PortsState {
    /// Pending, no ports.
    pub fn new(host_id: &str) -> Arc<Self> {
        Arc::new(Self {
            host_id: host_id.to_string(),
            current: Mutex::new(PortsEvent {
                host_id: host_id.to_string(),
                status: PortsStatus::Pending,
                message: None,
                ports: Vec::new(),
            }),
        })
    }

    /// The last report.
    pub fn snapshot(&self) -> PortsEvent {
        lock(&self.current).clone()
    }

    fn set(
        &self,
        ui: &dyn UiEvents,
        status: PortsStatus,
        message: Option<String>,
        ports: Vec<civet_proto::ListeningPort>,
    ) {
        let e = PortsEvent {
            host_id: self.host_id.clone(),
            status,
            message,
            ports,
        };
        {
            let mut cur = lock(&self.current);
            if *cur == e {
                return;
            }
            *cur = e.clone();
        }
        ui.ports(e);
    }
}

/// Watch `session`'s ports until it closes: open the ports stream, forward
/// every update, re-open it when it ends while the session lives on (a new
/// connection after a reconnect). Stops for good on `Unsupported`.
pub async fn watch_ports(session: ClientSession, state: Arc<PortsState>, ui: Arc<dyn UiEvents>) {
    let mut link = session.link_state();
    loop {
        // Wait for a live connection.
        loop {
            match &*link.borrow_and_update() {
                LinkState::Connected { .. } => break,
                LinkState::Closed | LinkState::Failed { .. } => return,
                _ => {}
            }
            if link.changed().await.is_err() {
                return;
            }
        }
        let mut watch = match session.watch_ports().await {
            Ok(w) => w,
            Err(RequestError::Rpc(e)) if e.code == ErrorCode::Unsupported => {
                state.set(&*ui, PortsStatus::Unsupported, Some(e.message), Vec::new());
                return;
            }
            Err(RequestError::Closed) => return,
            Err(e) => {
                tracing::debug!(error = %e, "ports watch not opened");
                tokio::time::sleep(REWATCH_PAUSE).await;
                continue;
            }
        };
        let mut got_any = false;
        loop {
            match watch.next().await {
                Ok(Some(PortsUpdate { ports })) => {
                    got_any = true;
                    state.set(&*ui, PortsStatus::Watching, None, ports);
                }
                Ok(None) => break,
                Err(e) => {
                    let alive =
                        matches!(&*session.link_state().borrow(), LinkState::Connected { .. });
                    if !got_any && alive {
                        // Reset at once on a live connection: the daemon
                        // does not know the ports stream.
                        tracing::info!(error = %e, "daemon does not report ports");
                        state.set(
                            &*ui,
                            PortsStatus::Unsupported,
                            Some(
                                "this daemon does not report ports (restart it to upgrade)".into(),
                            ),
                            Vec::new(),
                        );
                        return;
                    }
                    tracing::debug!(error = %e, "ports watch ended");
                    break;
                }
            }
        }
        tokio::time::sleep(REWATCH_PAUSE).await;
    }
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

struct Fwd {
    local: u16,
    connections: Arc<AtomicU32>,
    last_error: Arc<Mutex<Option<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Fwd {
    fn drop(&mut self) {
        // Aborting the accept loop drops its JoinSet, which aborts every
        // open connection of this forward.
        self.task.abort();
    }
}

/// Every forward of one host.
pub struct Forwards {
    host_id: String,
    session: SessionGetter,
    ui: Arc<dyn UiEvents>,
    rt: Handle,
    map: Mutex<BTreeMap<u16, Fwd>>,
}

impl std::fmt::Debug for Forwards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forwards")
            .field("host_id", &self.host_id)
            .field("ports", &lock(&self.map).keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Forwards {
    /// No forwards yet.
    pub fn new(
        host_id: &str,
        session: SessionGetter,
        ui: Arc<dyn UiEvents>,
        rt: Handle,
    ) -> Arc<Self> {
        Arc::new(Self {
            host_id: host_id.to_string(),
            session,
            ui,
            rt,
            map: Mutex::new(BTreeMap::new()),
        })
    }

    /// Forward `remote` (a port on the host) to `127.0.0.1:local` on this
    /// Mac. `local: None`: the same port number if it is free (unless
    /// `same_machine`), else any free port. Forwarding a port that already
    /// is returns the existing forward (an error if it is on a different
    /// local port than asked for).
    ///
    /// `same_machine`: the host is this Mac, where `local == remote` would
    /// make the daemon connect to the forward's own listener.
    pub async fn add(
        self: &Arc<Self>,
        remote: u16,
        local: Option<u16>,
        same_machine: bool,
    ) -> Result<ForwardView, CmdError> {
        if remote == 0 || local == Some(0) {
            return Err(CmdError::code(ErrorCode::InvalidArgument, "port 0"));
        }
        if same_machine && local == Some(remote) {
            return Err(CmdError::code(
                ErrorCode::InvalidArgument,
                format!("port {remote} is on this Mac already"),
            ));
        }
        if let Some(existing) = self.view(remote) {
            if local.is_some_and(|l| l != existing.local_port) {
                return Err(CmdError::code(
                    ErrorCode::AlreadyExists,
                    format!(
                        "port {remote} is already forwarded to localhost:{}",
                        existing.local_port
                    ),
                ));
            }
            return Ok(existing);
        }
        let listener = bind_local(remote, local, same_machine).await?;
        let local_port = listener.local_addr().map_err(io_err)?.port();
        let connections = Arc::new(AtomicU32::new(0));
        let last_error = Arc::new(Mutex::new(None));
        let task = self.rt.spawn(accept_loop(
            Arc::downgrade(self),
            listener,
            remote,
            connections.clone(),
            last_error.clone(),
        ));
        {
            let mut map = lock(&self.map);
            if map.contains_key(&remote) {
                // Lost a race with a concurrent add of the same port.
                task.abort();
                drop(map);
                return self
                    .view(remote)
                    .ok_or_else(|| CmdError::code(ErrorCode::Internal, "forward vanished"));
            }
            map.insert(
                remote,
                Fwd {
                    local: local_port,
                    connections,
                    last_error,
                    task,
                },
            );
        }
        tracing::info!(host = %self.host_id, remote, local = local_port, "port forwarded");
        self.publish();
        Ok(self.view(remote).expect("just inserted"))
    }

    /// Stop forwarding `remote` (closes its listener and connections).
    pub fn remove(&self, remote: u16) -> bool {
        let removed = lock(&self.map).remove(&remote);
        let found = removed.is_some();
        drop(removed);
        if found {
            tracing::info!(host = %self.host_id, remote, "port forward stopped");
            self.publish();
        }
        found
    }

    /// Stop everything (host disconnected).
    pub fn clear(&self) {
        let all = std::mem::take(&mut *lock(&self.map));
        let any = !all.is_empty();
        drop(all);
        if any {
            self.publish();
        }
    }

    /// All forwards, by remote port.
    pub fn list(&self) -> Vec<ForwardView> {
        lock(&self.map)
            .iter()
            .map(|(r, f)| view_of(*r, f))
            .collect()
    }

    fn view(&self, remote: u16) -> Option<ForwardView> {
        lock(&self.map).get(&remote).map(|f| view_of(remote, f))
    }

    fn publish(&self) {
        self.ui.forwards(ForwardsEvent {
            host_id: self.host_id.clone(),
            forwards: self.list(),
        });
    }
}

fn view_of(remote: u16, f: &Fwd) -> ForwardView {
    ForwardView {
        remote_port: remote,
        local_port: f.local,
        connections: f.connections.load(Ordering::Relaxed),
        last_error: lock(&f.last_error).clone(),
    }
}

fn io_err(e: std::io::Error) -> CmdError {
    CmdError::code(ErrorCode::Io, e.to_string())
}

async fn bind_local(
    remote: u16,
    local: Option<u16>,
    same_machine: bool,
) -> Result<TcpListener, CmdError> {
    let at = |p: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, p));
    match local {
        Some(p) => TcpListener::bind(at(p)).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                CmdError::code(
                    ErrorCode::AlreadyExists,
                    format!("localhost:{p} is already in use on this Mac"),
                )
            } else {
                CmdError::code(
                    ErrorCode::Io,
                    format!("cannot listen on localhost:{p}: {e}"),
                )
            }
        }),
        None if same_machine => TcpListener::bind(at(0)).await.map_err(io_err),
        None => match TcpListener::bind(at(remote)).await {
            Ok(l) => Ok(l),
            Err(_) => TcpListener::bind(at(0)).await.map_err(io_err),
        },
    }
}

async fn accept_loop(
    owner: std::sync::Weak<Forwards>,
    listener: TcpListener,
    remote: u16,
    connections: Arc<AtomicU32>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, _) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!(remote, error = %e, "accept failed");
                        tokio::time::sleep(RETRY_PAUSE).await;
                        continue;
                    }
                };
                let Some(fw) = owner.upgrade() else { return };
                conns.spawn(connection(fw, tcp, remote, connections.clone(), last_error.clone()));
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// Pipe one local connection through a new forward stream.
async fn connection(
    fw: Arc<Forwards>,
    mut tcp: TcpStream,
    remote: u16,
    connections: Arc<AtomicU32>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    let _ = tcp.set_nodelay(true);
    let set_error = |e: Option<String>| {
        let changed = {
            let mut cur = lock(&last_error);
            let changed = *cur != e;
            *cur = e;
            changed
        };
        if changed {
            fw.publish();
        }
    };
    let deadline = tokio::time::Instant::now() + CONNECT_PATIENCE;
    let mut tunnel = loop {
        let attempt = match (fw.session)() {
            Some(s) => s.open_forward(remote).await,
            None => Err(RequestError::ConnectionLost),
        };
        match attempt {
            Ok(t) => break t,
            Err(RequestError::ConnectionLost) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            Err(e) => {
                let message = match e {
                    RequestError::Rpc(rpc) => rpc.message,
                    RequestError::ConnectionLost => format!("{} is not reachable", fw.host_id),
                    other => other.to_string(),
                };
                tracing::debug!(host = %fw.host_id, remote, %message, "forward failed");
                set_error(Some(message));
                return;
            }
        }
    };
    set_error(None);
    connections.fetch_add(1, Ordering::Relaxed);
    fw.publish();
    let r = tokio::io::copy_bidirectional(&mut tcp, &mut tunnel).await;
    connections.fetch_sub(1, Ordering::Relaxed);
    fw.publish();
    if let Err(e) = r {
        tracing::debug!(host = %fw.host_id, remote, error = %e, "forwarded connection ended");
    }
}
