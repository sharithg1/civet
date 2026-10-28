//! HostManager: one [`ClientSession`] per host, the per-host event task
//! (screen updates → pane views, daemon events → UI, link state → host
//! state with the 1 s "reconnecting" rule, automatic re-bootstrap), and the
//! logic behind every command. Independent of Tauri: the app plugs in a
//! [`UiEvents`] and a [`Connector`]; tests plug in recorders and an
//! in-process daemon.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use civet_proto::{
    AgentStatus, AgentStatusEvent, AgentStatusSource, Event, KillSignal, LaunchAgentRequest,
    PaneId, PaneInfo, Request, Response, ScrollbackQuery, SpawnPaneRequest, SubscribeInfo,
    TermSize, WorktreeId,
};
use civet_transport::{ClientSession, LinkState, SessionEvent, TransportKind};
use futures::future::BoxFuture;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::batch::RecordSink;
use crate::error::CmdError;
use crate::events::{
    AgentAttentionEvent, BootstrapProgressEvent, ClipboardWrite, DaemonEventPayload,
    DaemonInfoView, DaemonUpgrade, ForwardView, HostState, HostStateEvent, HostSummary, HostTarget,
    LaunchedAgent, PingResult, PortsEvent, ProgressStep, RepoView, ScrollbackPageView,
    TransportChoice, UiEvents,
};
use crate::forwards::{Forwards, PortsState};
use crate::linkstate::{HostStateUpdate, LinkAction, LinkMapper, RECONNECTING_AFTER};
use crate::pane::PaneView;
use crate::predict::DEFAULT_MIN_RTT;

/// Reports bootstrap progress (step, detail).
pub type Progress = Arc<dyn Fn(ProgressStep, Option<String>) + Send + Sync>;

/// A connected session, as produced by a [`Connector`].
#[derive(Debug)]
pub struct Established {
    /// The session (first dial done).
    pub session: ClientSession,
    /// Its events.
    pub events: UnboundedReceiver<SessionEvent>,
    /// Link kind it dialed.
    pub transport: TransportKind,
    /// Daemon version from the bootstrap line / hello.
    pub daemon_version: String,
    /// Transport to use for the next (re-)bootstrap of this host (remembers
    /// an `auto` fallback for the app session).
    pub choice: TransportChoice,
    /// The bootstrap kept an older daemon with live panes although a newer
    /// one is installed ([`DaemonUpgrade::from_bootstrap`]).
    pub upgrade: Option<DaemonUpgrade>,
}

/// Bootstraps the daemon and dials the first connection (ssh + QUIC with
/// stdio-tunnel fallback in the app; an in-process daemon in tests).
pub trait Connector: Send + Sync + 'static {
    /// Connect; report steps through `progress`. `replace`: restart a
    /// running daemon even if it has live panes (`civetd bootstrap
    /// --replace`, the "Restart daemon" action).
    fn connect<'a>(
        &'a self,
        target: &'a HostTarget,
        choice: TransportChoice,
        replace: bool,
        progress: Progress,
    ) -> BoxFuture<'a, Result<Established, CmdError>>;
}

/// Tunables (defaults per docs/INTERFACES.md).
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    /// Show "reconnecting" after this much recovery.
    pub reconnecting_after: Duration,
    /// Keep a detached view (and its subscription) this long for an instant
    /// re-attach.
    pub detach_linger: Duration,
    /// Predict only above this RTT.
    pub predict_min_rtt: Duration,
    /// At most one native notification per (pane, status) per this
    /// interval.
    pub notify_interval: Duration,
    /// At most one OSC 52 clipboard write per pane per this interval.
    pub clipboard_interval: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            reconnecting_after: RECONNECTING_AFTER,
            detach_linger: Duration::from_secs(30),
            predict_min_rtt: DEFAULT_MIN_RTT,
            notify_interval: Duration::from_secs(10),
            clipboard_interval: Duration::from_secs(1),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

type ViewRef = Arc<Mutex<PaneView>>;

#[derive(Debug)]
struct Status {
    state: HostState,
    transport: Option<TransportKind>,
    rtt_ms: Option<u32>,
    daemon_version: Option<String>,
    instance_id: Option<u64>,
    upgrade: Option<DaemonUpgrade>,
}

struct Host {
    id: String,
    label: String,
    target: HostTarget,
    choice: Mutex<TransportChoice>,
    session: Mutex<Option<ClientSession>>,
    status: Mutex<Status>,
    views: Mutex<HashMap<PaneId, ViewRef>>,
    panes: Mutex<HashMap<PaneId, PaneInfo>>,
    /// Last native notification per (pane, status).
    notified: Mutex<HashMap<(PaneId, AgentStatus), Instant>>,
    /// Last applied OSC 52 clipboard write per pane.
    clipboard_at: Mutex<HashMap<PaneId, Instant>>,
    /// Per-pane lock serializing subscribe (attach) and the linger
    /// unsubscribe, so an expiring linger never cancels a new attach.
    sub_locks: Mutex<HashMap<PaneId, Arc<tokio::sync::Mutex<()>>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Events of a freshly installed session, until the host task takes them.
    pending_events: Mutex<Option<UnboundedReceiver<SessionEvent>>>,
    /// Listening ports, as last reported.
    ports: Arc<PortsState>,
    /// Watches the current session's ports.
    ports_task: Mutex<Option<JoinHandle<()>>>,
    /// Port forwards (outlive sessions, not the host).
    forwards: Arc<Forwards>,
    closed: AtomicBool,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Host {
    fn summary(&self) -> HostSummary {
        let st = lock(&self.status);
        HostSummary {
            host_id: self.id.clone(),
            label: self.label.clone(),
            state: st.state,
            transport: st.transport,
            daemon_version: st.daemon_version.clone(),
            instance_id: st.instance_id.map(|i| i.to_string()),
            upgrade: st.upgrade.clone(),
        }
    }

    fn sub_lock(&self, pane: PaneId) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.sub_locks).entry(pane).or_default().clone()
    }

    fn session(&self) -> Result<ClientSession, CmdError> {
        lock(&self.session)
            .clone()
            .ok_or_else(|| CmdError::transport(format!("host {} is not connected", self.id)))
    }

    fn view(&self, pane: PaneId) -> Option<ViewRef> {
        lock(&self.views).get(&pane).cloned()
    }

    /// Mark every view's pane as gone (daemon restarted) and forget them.
    fn close_all_views(&self) {
        let views: Vec<ViewRef> = lock(&self.views).drain().map(|(_, v)| v).collect();
        for v in views {
            lock(&v).close();
        }
        lock(&self.panes).clear();
    }
}

struct Inner {
    cfg: ManagerConfig,
    ui: Arc<dyn UiEvents>,
    connector: Arc<dyn Connector>,
    rt: Handle,
    hosts: Mutex<HashMap<String, Arc<Host>>>,
    predict: AtomicBool,
    nonce: AtomicU64,
}

/// Owns every host. Clone = same manager.
#[derive(Clone)]
pub struct HostManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HostManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hosts: Vec<String> = lock(&self.inner.hosts).keys().cloned().collect();
        f.debug_struct("HostManager")
            .field("hosts", &hosts)
            .finish()
    }
}

macro_rules! expect_response {
    ($resp:expr, $($pat:pat => $val:expr),+) => {
        match $resp {
            $($pat => Ok($val),)+
            other => Err(CmdError::code(
                civet_proto::ErrorCode::Internal,
                format!("unexpected response: {other:?}"),
            )),
        }
    };
}

impl HostManager {
    /// New manager; background tasks are spawned on `rt`.
    pub fn new(
        cfg: ManagerConfig,
        ui: Arc<dyn UiEvents>,
        connector: Arc<dyn Connector>,
        rt: Handle,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                ui,
                connector,
                rt,
                hosts: Mutex::new(HashMap::new()),
                predict: AtomicBool::new(true),
                nonce: AtomicU64::new(1),
            }),
        }
    }

    fn host(&self, host_id: &str) -> Result<Arc<Host>, CmdError> {
        lock(&self.inner.hosts)
            .get(host_id)
            .cloned()
            .ok_or_else(|| CmdError::no_such_host(host_id))
    }

    fn session(&self, host_id: &str) -> Result<ClientSession, CmdError> {
        self.host(host_id)?.session()
    }

    async fn request(&self, host_id: &str, req: Request) -> Result<Response, CmdError> {
        Ok(self.session(host_id)?.request(req).await?)
    }

    fn publish(&self, host: &Host, u: HostStateUpdate) {
        let upgrade = {
            let mut st = lock(&host.status);
            st.state = u.state;
            st.transport = u.transport.or(match u.state {
                HostState::Disconnected | HostState::Failed => None,
                _ => st.transport,
            });
            st.rtt_ms = u.rtt_ms;
            st.upgrade.clone()
        };
        self.inner.ui.host_state(HostStateEvent {
            host_id: host.id.clone(),
            state: u.state,
            transport: u.transport,
            rtt_ms: u.rtt_ms,
            message: u.message,
            upgrade,
        });
    }

    fn progress_for(&self, host_id: &str) -> Progress {
        let ui = self.inner.ui.clone();
        let host_id = host_id.to_string();
        Arc::new(move |step, message| {
            ui.bootstrap_progress(BootstrapProgressEvent {
                host_id: host_id.clone(),
                step,
                message,
            })
        })
    }

    // ----- hosts -----

    /// `connect_host`: bootstrap + first connection. Idempotent while the
    /// host is connected/connecting.
    pub async fn connect_host(
        &self,
        target: HostTarget,
        choice: TransportChoice,
    ) -> Result<HostSummary, CmdError> {
        self.connect_inner(target, choice, false).await
    }

    /// `reconnect_host`: connect again with the host's stored target (ssh
    /// port, extra args) and transport choice. Idempotent while connected.
    /// `"local"` works even if the host was never connected.
    pub async fn reconnect_host(&self, host_id: &str) -> Result<HostSummary, CmdError> {
        let (target, choice) = match self.host(host_id) {
            Ok(h) => (h.target.clone(), *lock(&h.choice)),
            Err(_) if host_id == "local" => (HostTarget::Local, TransportChoice::Auto),
            Err(e) => return Err(e),
        };
        self.connect_inner(target, choice, false).await
    }

    /// `restart_daemon`: re-bootstrap the host with `replace`, restarting
    /// its daemon on this client's version even though it has live panes
    /// (they are closed). Used for [`HostSummary::upgrade`] and for an
    /// incompatible busy daemon (`daemonBusy`).
    pub async fn restart_daemon(&self, host_id: &str) -> Result<HostSummary, CmdError> {
        let (target, choice) = match self.host(host_id) {
            Ok(h) => {
                let t = (h.target.clone(), *lock(&h.choice));
                self.shutdown_host(&h).await;
                t
            }
            Err(_) if host_id == "local" => (HostTarget::Local, TransportChoice::Auto),
            Err(e) => return Err(e),
        };
        tracing::info!(host = %host_id, "restarting the daemon (replace)");
        self.connect_inner(target, choice, true).await
    }

    async fn connect_inner(
        &self,
        target: HostTarget,
        choice: TransportChoice,
        replace: bool,
    ) -> Result<HostSummary, CmdError> {
        let id = target.host_id();
        if id.is_empty() {
            return Err(CmdError::code(
                civet_proto::ErrorCode::InvalidArgument,
                "empty ssh destination",
            ));
        }
        let previous = lock(&self.inner.hosts).get(&id).cloned();
        if let Some(h) = previous {
            let state = lock(&h.status).state;
            if matches!(
                state,
                HostState::Connected | HostState::Connecting | HostState::Reconnecting
            ) {
                return Ok(h.summary());
            }
            self.shutdown_host(&h).await;
        }
        let host = Arc::new_cyclic(|weak: &std::sync::Weak<Host>| Host {
            ports: PortsState::new(&id),
            ports_task: Mutex::new(None),
            forwards: Forwards::new(
                &id,
                {
                    let weak = weak.clone();
                    Arc::new(move || weak.upgrade().and_then(|h| lock(&h.session).clone()))
                },
                self.inner.ui.clone(),
                self.inner.rt.clone(),
            ),
            id: id.clone(),
            label: target.label(),
            target: target.clone(),
            choice: Mutex::new(choice),
            session: Mutex::new(None),
            status: Mutex::new(Status {
                state: HostState::Connecting,
                transport: None,
                rtt_ms: None,
                daemon_version: None,
                instance_id: None,
                upgrade: None,
            }),
            views: Mutex::new(HashMap::new()),
            panes: Mutex::new(HashMap::new()),
            notified: Mutex::new(HashMap::new()),
            clipboard_at: Mutex::new(HashMap::new()),
            sub_locks: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
            pending_events: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        lock(&self.inner.hosts).insert(id.clone(), host.clone());
        self.publish(
            &host,
            HostStateUpdate {
                state: HostState::Connecting,
                transport: None,
                rtt_ms: None,
                message: None,
            },
        );
        let res = self
            .inner
            .connector
            .connect(&target, choice, replace, self.progress_for(&id))
            .await;
        let still_ours = lock(&self.inner.hosts)
            .get(&id)
            .is_some_and(|h| Arc::ptr_eq(h, &host));
        match res {
            Err(e) => {
                if still_ours {
                    let mut m = LinkMapper::new(self.inner.cfg.reconnecting_after);
                    self.publish(&host, m.fail(e.message.clone()));
                }
                Err(e)
            }
            Ok(est) => {
                if !still_ours || host.closed.load(Ordering::SeqCst) {
                    est.session.close().await;
                    return Err(CmdError::transport("disconnected while connecting"));
                }
                let transport = est.transport;
                self.install(&host, est);
                let task = self
                    .inner
                    .rt
                    .spawn(self.clone().host_task(host.clone(), transport));
                *lock(&host.task) = Some(task);
                Ok(host.summary())
            }
        }
    }

    /// Adopt a fresh session for `host`; starts its event pump.
    fn install(&self, host: &Arc<Host>, est: Established) {
        let hello = est.session.server_hello();
        *lock(&host.choice) = est.choice;
        {
            let mut st = lock(&host.status);
            st.daemon_version = Some(est.daemon_version.clone());
            st.instance_id = Some(hello.instance_id);
            st.transport = Some(est.transport);
            st.upgrade = est.upgrade.clone();
            if let Some(u) = &est.upgrade {
                tracing::info!(
                    host = %host.id,
                    running = %u.running_version,
                    available = %u.available_version,
                    live_panes = u.live_panes,
                    "older daemon kept (live panes); restart_daemon upgrades it"
                );
            }
        }
        *lock(&host.session) = Some(est.session.clone());
        *lock(&host.pending_events) = Some(est.events);
        let watcher = self.inner.rt.spawn(crate::forwards::watch_ports(
            est.session.clone(),
            host.ports.clone(),
            self.inner.ui.clone(),
        ));
        if let Some(old) = lock(&host.ports_task).replace(watcher) {
            old.abort();
        }
        self.publish(
            host,
            HostStateUpdate {
                state: HostState::Connected,
                transport: Some(est.transport),
                rtt_ms: None,
                message: None,
            },
        );
        self.refresh_panes(host, est.session);
    }

    fn refresh_panes(&self, host: &Arc<Host>, session: ClientSession) {
        let host = host.clone();
        self.inner.rt.spawn(async move {
            if let Ok(Response::Panes(list)) = session.request(Request::ListPanes).await {
                *lock(&host.panes) = list.into_iter().map(|p| (p.id, p)).collect();
            }
        });
    }

    async fn host_task(self, host: Arc<Host>, transport: TransportKind) {
        let Some(mut events) = lock(&host.pending_events).take() else {
            return;
        };
        let mut sm = LinkMapper::connected(self.inner.cfg.reconnecting_after, transport);
        loop {
            let deadline = sm.deadline();
            tokio::select! {
                ev = events.recv() => {
                    let Some(ev) = ev else {
                        // Supervisor gone. Keep a "failed" state (the user
                        // must reconnect); otherwise report "disconnected".
                        if !host.closed.load(Ordering::SeqCst)
                            && sm.shown() != HostState::Failed
                            && let Some(LinkAction::Publish(u)) = sm.on_link(&LinkState::Closed, Instant::now())
                        {
                            self.publish(&host, u);
                        }
                        return;
                    };
                    match ev {
                        SessionEvent::Link(ls) => match sm.on_link(&ls, Instant::now()) {
                            Some(LinkAction::Publish(u)) => self.publish(&host, u),
                            Some(LinkAction::Rebootstrap { reason }) => {
                                if host.closed.load(Ordering::SeqCst) {
                                    return;
                                }
                                match self.rebootstrap(&host, &mut sm, reason).await {
                                    Some(ev) => events = ev,
                                    None => return,
                                }
                            }
                            None => {}
                        },
                        other => self.handle_event(&host, other),
                    }
                }
                () = sleep_until_opt(deadline), if deadline.is_some() => {
                    if let Some(u) = sm.on_timer(Instant::now()) {
                        self.publish(&host, u);
                    }
                }
            }
        }
    }

    /// Re-run bootstrap once (daemon restarted / said goodbye). Returns the
    /// new session's events, or None after publishing `failed`.
    async fn rebootstrap(
        &self,
        host: &Arc<Host>,
        sm: &mut LinkMapper,
        reason: String,
    ) -> Option<UnboundedReceiver<SessionEvent>> {
        tracing::info!(host = %host.id, %reason, "re-bootstrapping");
        let old_instance = lock(&host.status).instance_id;
        let old_session = lock(&host.session).take();
        if let Some(s) = old_session {
            s.close().await;
        }
        let choice = *lock(&host.choice);
        let fut =
            self.inner
                .connector
                .connect(&host.target, choice, false, self.progress_for(&host.id));
        tokio::pin!(fut);
        let res = loop {
            let deadline = sm.deadline();
            tokio::select! {
                r = &mut fut => break r,
                () = sleep_until_opt(deadline), if deadline.is_some() => {
                    if let Some(u) = sm.on_timer(Instant::now()) {
                        self.publish(host, u);
                    }
                }
            }
        };
        match res {
            Err(e) => {
                tracing::warn!(host = %host.id, error = %e, "re-bootstrap failed");
                self.publish(
                    host,
                    sm.fail(format!("{reason}; re-bootstrap failed: {}", e.message)),
                );
                None
            }
            Ok(est) => {
                if host.closed.load(Ordering::SeqCst) {
                    est.session.close().await;
                    return None;
                }
                let transport = est.transport;
                let session = est.session.clone();
                let new_instance = session.server_hello().instance_id;
                self.install(host, est);
                let events = lock(&host.pending_events).take();
                // Forgets the old RTT so the new session's first RTT sample
                // is published (and prediction turns back on).
                sm.reconnected(transport);
                if old_instance != Some(new_instance) {
                    host.close_all_views();
                } else {
                    self.resubscribe_all(host, &session);
                }
                events
            }
        }
    }

    fn resubscribe_all(&self, host: &Arc<Host>, session: &ClientSession) {
        let views: Vec<(PaneId, ViewRef)> = lock(&host.views)
            .iter()
            .map(|(p, v)| (*p, v.clone()))
            .collect();
        for (pane, view) in views {
            lock(&view).set_subscribed(false);
            let session = session.clone();
            let host = host.clone();
            self.inner.rt.spawn(async move {
                let sub = host.sub_lock(pane);
                let _guard = sub.lock().await;
                match session.subscribe(pane).await {
                    Ok(_) => lock(&view).set_subscribed(true),
                    Err(_) => {
                        lock(&view).close();
                        let mut views = lock(&host.views);
                        if views.get(&pane).is_some_and(|v| Arc::ptr_eq(v, &view)) {
                            views.remove(&pane);
                        }
                    }
                }
            });
        }
    }

    fn handle_event(&self, host: &Arc<Host>, ev: SessionEvent) {
        match ev {
            SessionEvent::Screen(u) => {
                if let Some(v) = host.view(u.pane) {
                    lock(&v).on_screen(&u);
                }
            }
            SessionEvent::InputAcked { pane, seq } => {
                if let Some(v) = host.view(pane) {
                    lock(&v).on_input_ack(seq);
                }
            }
            SessionEvent::Event(event) => self.handle_daemon_event(host, event),
            SessionEvent::Connected {
                resumed,
                instance_changed,
                server_hello,
            } => {
                tracing::info!(host = %host.id, resumed, instance_changed, "connection (re)established");
                lock(&host.status).instance_id = Some(server_hello.instance_id);
                if instance_changed {
                    host.close_all_views();
                } else if !resumed {
                    // Fresh session: input numbering restarted at 1 and old
                    // guesses can never be acked.
                    let views: Vec<ViewRef> = lock(&host.views).values().cloned().collect();
                    for v in views {
                        lock(&v).reset_input();
                    }
                }
                if let Ok(s) = host.session() {
                    self.refresh_panes(host, s);
                }
            }
            SessionEvent::Goodbye { reason } => {
                tracing::info!(host = %host.id, %reason, "daemon said goodbye");
            }
            SessionEvent::Link(_) => {}
        }
    }

    fn handle_daemon_event(&self, host: &Arc<Host>, event: Event) {
        // OSC 52: straight to the clipboard; the text is not broadcast to
        // the UI as a daemon event.
        if let Event::Clipboard { pane, text } = event {
            if text.len() > civet_proto::MAX_CLIPBOARD_BYTES {
                tracing::warn!(%pane, len = text.len(), "oversized clipboard write ignored");
                return;
            }
            if !self.clipboard_allowed(host, pane) {
                return;
            }
            self.inner.ui.clipboard(ClipboardWrite {
                host_id: host.id.clone(),
                pane,
                text,
            });
            return;
        }
        match &event {
            Event::PaneCreated(info) => {
                lock(&host.panes).insert(info.id, info.clone());
            }
            Event::PaneTitle { pane, title } => {
                if let Some(p) = lock(&host.panes).get_mut(pane) {
                    p.title.clone_from(title);
                }
            }
            Event::PaneResized { pane, size } => {
                if let Some(p) = lock(&host.panes).get_mut(pane) {
                    p.size = *size;
                }
            }
            Event::PaneExited { pane, exit_code } => {
                if let Some(p) = lock(&host.panes).get_mut(pane) {
                    p.exited = true;
                    p.exit_code = *exit_code;
                }
            }
            Event::PaneClosed { pane } => {
                lock(&host.panes).remove(pane);
                lock(&host.notified).retain(|(p, _), _| p != pane);
                lock(&host.clipboard_at).remove(pane);
                if let Some(v) = lock(&host.views).remove(pane) {
                    lock(&v).close();
                }
            }
            Event::AgentStatus(e) => {
                if let Some(p) = lock(&host.panes).get_mut(&e.pane) {
                    p.status = e.status;
                }
                self.agent_attention(host, e);
            }
            Event::Bell { .. }
            | Event::ReposChanged
            | Event::WorktreesChanged { .. }
            | Event::Clipboard { .. } => {}
        }
        self.inner.ui.daemon_event(DaemonEventPayload {
            host_id: host.id.clone(),
            event,
        });
    }

    /// OSC 52 policy (security): a program may set the clipboard only while
    /// a Civet window is focused and its pane is attached (visible) in the
    /// UI, at most once per [`ManagerConfig::clipboard_interval`]. Anything
    /// else is dropped (a background pane cannot silently replace what the
    /// user is about to paste).
    fn clipboard_allowed(&self, host: &Host, pane: PaneId) -> bool {
        if !host.view(pane).is_some_and(|v| lock(&v).is_attached()) {
            tracing::debug!(host = %host.id, %pane, "OSC 52 dropped: pane not visible");
            return false;
        }
        if !self.inner.ui.window_focused() {
            tracing::debug!(host = %host.id, %pane, "OSC 52 dropped: window not focused");
            return false;
        }
        let now = Instant::now();
        let mut last = lock(&host.clipboard_at);
        if last
            .get(&pane)
            .is_some_and(|t| now.duration_since(*t) < self.inner.cfg.clipboard_interval)
        {
            tracing::debug!(host = %host.id, %pane, "OSC 52 dropped: rate limited");
            return false;
        }
        last.insert(pane, now);
        true
    }

    fn agent_attention(&self, host: &Arc<Host>, e: &AgentStatusEvent) {
        if !matches!(e.status, AgentStatus::Done | AgentStatus::NeedsInput)
            || e.previous == e.status
        {
            return;
        }
        let (title, worktree) = {
            let panes = lock(&host.panes);
            let info = panes.get(&e.pane);
            // Only agents ask for attention: a pane launched as an agent, or
            // one whose agent reports through its hook (`claude` started by
            // hand in a shell). A plain shell exiting is not news.
            let is_agent =
                info.is_some_and(|p| p.agent.is_some()) || e.source == AgentStatusSource::Hook;
            if !is_agent {
                return;
            }
            let worktree = info.and_then(|p| p.worktree.as_ref()).map(|w| w.0.clone());
            let title = info
                .map(|p| p.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    worktree.as_deref().and_then(|w| {
                        Path::new(w)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    })
                })
                .unwrap_or_else(|| format!("Pane {}", e.pane));
            (title, worktree)
        };
        self.inner.ui.agent_attention(AgentAttentionEvent {
            host_id: host.id.clone(),
            pane: e.pane,
            status: e.status,
            previous: e.previous,
            title: title.clone(),
            worktree,
            message: e.message.clone(),
        });
        if self.inner.ui.window_focused() {
            return;
        }
        let now = Instant::now();
        {
            let mut n = lock(&host.notified);
            let key = (e.pane, e.status);
            if n.get(&key)
                .is_some_and(|t| now.duration_since(*t) < self.inner.cfg.notify_interval)
            {
                return;
            }
            n.insert(key, now);
        }
        let body = e
            .message
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| match e.status {
                AgentStatus::Done => "Agent finished".to_string(),
                _ => "Agent needs input".to_string(),
            });
        self.inner.ui.notify(&title, &body);
    }

    async fn shutdown_host(&self, host: &Arc<Host>) {
        host.closed.store(true, Ordering::SeqCst);
        let session = lock(&host.session).take();
        if let Some(s) = session {
            s.close().await;
        }
        if let Some(t) = lock(&host.task).take() {
            t.abort();
        }
        if let Some(t) = lock(&host.ports_task).take() {
            t.abort();
        }
        host.forwards.clear();
        let views: Vec<ViewRef> = lock(&host.views).drain().map(|(_, v)| v).collect();
        for v in views {
            lock(&v).detach();
        }
        lock(&host.pending_events).take();
        lock(&self.inner.hosts).retain(|_, h| !Arc::ptr_eq(h, host));
    }

    /// `disconnect_host`: Goodbye, close, forget the host.
    pub async fn disconnect_host(&self, host_id: &str) -> Result<(), CmdError> {
        let host = self.host(host_id)?;
        self.shutdown_host(&host).await;
        self.publish(
            &host,
            HostStateUpdate {
                state: HostState::Disconnected,
                transport: None,
                rtt_ms: None,
                message: None,
            },
        );
        Ok(())
    }

    /// `list_hosts`
    pub fn list_hosts(&self) -> Vec<HostSummary> {
        let mut v: Vec<HostSummary> = lock(&self.inner.hosts)
            .values()
            .map(|h| h.summary())
            .collect();
        v.sort_by(|a, b| a.host_id.cmp(&b.host_id));
        v
    }

    /// `notify_network_change` (and the wake detector): rebind + ping every
    /// session so the next packet is the reconnect.
    pub fn notify_network_change(&self) {
        let sessions: Vec<ClientSession> = lock(&self.inner.hosts)
            .values()
            .filter_map(|h| lock(&h.session).clone())
            .collect();
        for s in sessions {
            s.notify_network_change();
        }
    }

    /// Close every session (app exit).
    pub async fn shutdown(&self) {
        let hosts: Vec<Arc<Host>> = lock(&self.inner.hosts).values().cloned().collect();
        for h in hosts {
            self.shutdown_host(&h).await;
        }
    }

    // ----- daemon RPCs -----

    /// `daemon_info`
    pub async fn daemon_info(&self, host_id: &str) -> Result<DaemonInfoView, CmdError> {
        expect_response!(self.request(host_id, Request::GetDaemonInfo).await?, Response::DaemonInfo(i) => i.into())
    }

    /// `ping`
    pub async fn ping(&self, host_id: &str) -> Result<PingResult, CmdError> {
        let session = self.session(host_id)?;
        let nonce = self.inner.nonce.fetch_add(1, Ordering::Relaxed);
        let t = std::time::Instant::now();
        let resp = session.request(Request::Ping { nonce }).await?;
        let rtt = t.elapsed();
        expect_response!(resp, Response::Pong { .. } => PingResult {
            rtt_ms: u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX),
        })
    }

    /// `list_repos`: every project (`ListProjects`), or only repositories
    /// on a daemon older than [`civet_proto::PROJECTS_MIN_DAEMON_VERSION`].
    pub async fn list_repos(&self, host_id: &str) -> Result<Vec<RepoView>, CmdError> {
        if self.daemon_at_least(host_id, civet_proto::PROJECTS_MIN_DAEMON_VERSION)? {
            return expect_response!(
                self.request(host_id, Request::ListProjects).await?,
                Response::Projects(p) => p.into_iter().map(RepoView::from).collect()
            );
        }
        expect_response!(
            self.request(host_id, Request::ListRepos).await?,
            Response::Repos(r) => r.into_iter().map(RepoView::git).collect()
        )
    }

    /// `add_repo`: the git repository containing `path`, else (daemons of
    /// at least [`civet_proto::PROJECTS_MIN_DAEMON_VERSION`]) the folder
    /// itself. An older daemon refuses a folder; its error then says why.
    pub async fn add_repo(&self, host_id: &str, path: String) -> Result<RepoView, CmdError> {
        if self.daemon_at_least(host_id, civet_proto::PROJECTS_MIN_DAEMON_VERSION)? {
            return expect_response!(
                self.request(host_id, Request::AddProject { path }).await?,
                Response::Project(p) => p.into()
            );
        }
        match self.request(host_id, Request::AddRepo { path }).await {
            Ok(resp) => expect_response!(resp, Response::Repo(r) => RepoView::git(r)),
            Err(mut e)
                if e.code == "invalidArgument" && e.message.contains("not a git repository") =>
            {
                let running = lock(&self.host(host_id)?.status).daemon_version.clone();
                e.message = format!(
                    "{}. civetd {} on {host_id} only adds git repositories; v{} or newer also adds plain folders (restart the daemon to upgrade)",
                    e.message.trim_end_matches('.'),
                    running.as_deref().unwrap_or("?"),
                    civet_proto::PROJECTS_MIN_DAEMON_VERSION
                );
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// `remove_repo`
    pub async fn remove_repo(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
    ) -> Result<(), CmdError> {
        expect_response!(self.request(host_id, Request::RemoveRepo { repo }).await?, Response::Ok => ())
    }

    /// `prepare_file_drop`: make files dropped on a pane usable from
    /// `host_id`. On This Mac the local paths work as they are; for a remote
    /// host each file is uploaded (`UploadFile`) and the daemon-side path
    /// returned. One result per input path, in order.
    pub async fn prepare_file_drop(
        &self,
        host_id: &str,
        paths: Vec<String>,
    ) -> Result<Vec<DroppedFile>, CmdError> {
        if host_id == "local" {
            return Ok(paths
                .into_iter()
                .map(|p| DroppedFile {
                    path: Some(p.clone()),
                    source: p,
                    error: None,
                })
                .collect());
        }
        self.check_daemon_version(host_id, civet_proto::UPLOAD_FILE_MIN_DAEMON_VERSION)?;
        let mut out = Vec::with_capacity(paths.len());
        for source in paths {
            let r = self.upload_one(host_id, &source).await;
            out.push(match r {
                Ok(path) => DroppedFile {
                    source,
                    path: Some(path),
                    error: None,
                },
                Err(e) => DroppedFile {
                    source,
                    path: None,
                    error: Some(e.message),
                },
            });
        }
        Ok(out)
    }

    async fn upload_one(&self, host_id: &str, source: &str) -> Result<String, CmdError> {
        let io = |e: std::io::Error| CmdError::new("io", format!("{source}: {e}"));
        let meta = tokio::fs::metadata(source).await.map_err(io)?;
        if !meta.is_file() {
            return Err(CmdError::new(
                "invalidArgument",
                format!("{source}: only files can be dropped on a remote pane"),
            ));
        }
        if meta.len() > civet_proto::MAX_UPLOAD_BYTES as u64 {
            return Err(CmdError::new(
                "invalidArgument",
                format!(
                    "{source}: larger than {} MiB",
                    civet_proto::MAX_UPLOAD_BYTES >> 20
                ),
            ));
        }
        let data = tokio::fs::read(source).await.map_err(io)?;
        let name = Path::new(source)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.upload_file(host_id, name, data).await
    }

    /// `upload_file`: store `data` as `name` in a fresh temp directory on
    /// `host_id` (any host, This Mac included) and return its path there.
    /// Used for clipboard images pasted into a pane.
    pub async fn upload_file(
        &self,
        host_id: &str,
        name: String,
        data: Vec<u8>,
    ) -> Result<String, CmdError> {
        self.check_daemon_version(host_id, civet_proto::UPLOAD_FILE_MIN_DAEMON_VERSION)?;
        if data.len() > civet_proto::MAX_UPLOAD_BYTES {
            return Err(CmdError::new(
                "invalidArgument",
                format!(
                    "{name}: larger than {} MiB",
                    civet_proto::MAX_UPLOAD_BYTES >> 20
                ),
            ));
        }
        expect_response!(
            self.request(host_id, Request::UploadFile { name, data }).await?,
            Response::FileUploaded { path } => path
        )
    }

    /// Requests appended after `min` would make an older daemon drop the
    /// connection.
    /// True if the host's daemon is `min` or newer (false if unknown).
    fn daemon_at_least(&self, host_id: &str, min: &str) -> Result<bool, CmdError> {
        let running = lock(&self.host(host_id)?.status).daemon_version.clone();
        Ok(running.as_deref().is_some_and(|v| !version_older(v, min)))
    }

    fn check_daemon_version(&self, host_id: &str, min: &str) -> Result<(), CmdError> {
        let running = lock(&self.host(host_id)?.status).daemon_version.clone();
        if running.as_deref().is_none_or(|v| version_older(v, min)) {
            return Err(CmdError::new(
                "unsupported",
                format!(
                    "{host_id} runs civetd {}; this needs v{min} or newer (restart the daemon to upgrade)",
                    running.as_deref().unwrap_or("?")
                ),
            ));
        }
        Ok(())
    }

    // ----- ports -----

    /// `list_ports`: the host's listening ports as last reported (they
    /// also arrive as `civet://ports`).
    pub fn list_ports(&self, host_id: &str) -> Result<PortsEvent, CmdError> {
        Ok(self.host(host_id)?.ports.snapshot())
    }

    /// `forward_port`: listen on `127.0.0.1:local` (None: the same port if
    /// free, else any) and tunnel every connection to `remote` on the host.
    pub async fn forward_port(
        &self,
        host_id: &str,
        remote: u16,
        local: Option<u16>,
    ) -> Result<ForwardView, CmdError> {
        // Allowed for This Mac too (a second port number for a local
        // server); the UI only offers it for remote hosts.
        let same_machine = matches!(self.host(host_id)?.target, HostTarget::Local);
        self.host(host_id)?
            .forwards
            .add(remote, local, same_machine)
            .await
    }

    /// `stop_forward` (`notFound` if `remote` is not forwarded).
    pub fn stop_forward(&self, host_id: &str, remote: u16) -> Result<(), CmdError> {
        if self.host(host_id)?.forwards.remove(remote) {
            Ok(())
        } else {
            Err(CmdError::code(
                civet_proto::ErrorCode::NotFound,
                format!("port {remote} is not forwarded"),
            ))
        }
    }

    /// `list_forwards`
    pub fn list_forwards(&self, host_id: &str) -> Result<Vec<ForwardView>, CmdError> {
        Ok(self.host(host_id)?.forwards.list())
    }

    /// `list_worktrees`
    pub async fn list_worktrees(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
    ) -> Result<Vec<civet_proto::WorktreeInfo>, CmdError> {
        expect_response!(self.request(host_id, Request::ListWorktrees { repo }).await?, Response::Worktrees(w) => w)
    }

    /// `list_pull_requests`: `gh pr list` on the host.
    pub async fn list_pull_requests(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
    ) -> Result<Vec<civet_proto::PullRequestInfo>, CmdError> {
        self.check_daemon_version(host_id, civet_proto::LIST_PULL_REQUESTS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::ListPullRequests { repo }).await?, Response::PullRequests(p) => p)
    }

    /// `git_status`: the worktree's uncommitted changes.
    pub async fn git_status(
        &self,
        host_id: &str,
        worktree: WorktreeId,
    ) -> Result<civet_proto::GitStatusInfo, CmdError> {
        self.check_daemon_version(host_id, civet_proto::GIT_STATUS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::GitStatus { worktree }).await?, Response::GitStatus(s) => s)
    }

    /// `git_diff`: one file's unified diff.
    pub async fn git_diff(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        path: String,
        old_path: Option<String>,
        staged: bool,
        context_lines: u32,
    ) -> Result<civet_proto::GitDiffInfo, CmdError> {
        self.check_daemon_version(host_id, civet_proto::GIT_STATUS_MIN_DAEMON_VERSION)?;
        let request = Request::GitDiff {
            worktree,
            path,
            old_path,
            staged,
            context_lines,
        };
        expect_response!(self.request(host_id, request).await?, Response::GitDiff(d) => d)
    }

    /// `git_branch`: the worktree's branch, upstream and ahead / behind.
    pub async fn git_branch(
        &self,
        host_id: &str,
        worktree: WorktreeId,
    ) -> Result<civet_proto::GitBranchInfo, CmdError> {
        self.check_daemon_version(host_id, civet_proto::GIT_ACTIONS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::GitBranch { worktree }).await?, Response::GitBranch(b) => b)
    }

    /// `git_action`: a git write operation in the worktree. Returns the
    /// snapshot that undoes a discard, None for every other action.
    pub async fn git_action(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        action: civet_proto::GitAction,
    ) -> Result<Option<civet_proto::GitDiscardSnapshot>, CmdError> {
        self.check_daemon_version(host_id, civet_proto::GIT_ACTIONS_MIN_DAEMON_VERSION)?;
        expect_response!(
            self.request(host_id, Request::GitAction { worktree, action }).await?,
            Response::Ok => None,
            Response::GitDiscarded(s) => Some(s)
        )
    }

    /// `worktree_lifecycle`: ahead / behind and staleness of a repo's
    /// worktrees.
    pub async fn worktree_lifecycle(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
        fetch: bool,
        review: bool,
    ) -> Result<civet_proto::RepoLifecycle, CmdError> {
        self.check_daemon_version(host_id, civet_proto::WORKTREE_LIFECYCLE_MIN_DAEMON_VERSION)?;
        let request = Request::WorktreeLifecycle {
            repo,
            fetch,
            review,
        };
        expect_response!(self.request(host_id, request).await?, Response::WorktreeLifecycle(l) => l)
    }

    /// `update_worktree`: bring the default branch into the worktree.
    pub async fn update_worktree(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        strategy: civet_proto::UpdateStrategy,
    ) -> Result<civet_proto::WorktreeUpdate, CmdError> {
        self.check_daemon_version(host_id, civet_proto::WORKTREE_LIFECYCLE_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::UpdateWorktree { worktree, strategy }).await?, Response::WorktreeUpdated(u) => u)
    }

    /// `create_worktree`
    pub async fn create_worktree(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
        branch: String,
        base: Option<String>,
        path: Option<String>,
    ) -> Result<civet_proto::WorktreeInfo, CmdError> {
        let req = Request::CreateWorktree {
            repo,
            branch,
            base: base.filter(|s| !s.trim().is_empty()),
            path: path.filter(|s| !s.trim().is_empty()),
        };
        expect_response!(self.request(host_id, req).await?, Response::Worktree(w) => w)
    }

    /// `remove_worktree`
    pub async fn remove_worktree(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        force: bool,
    ) -> Result<(), CmdError> {
        expect_response!(self.request(host_id, Request::RemoveWorktree { worktree, force }).await?, Response::Ok => ())
    }

    /// `list_dirs`: subdirectories of any host directory, for completing
    /// the Add project folder field.
    pub async fn list_dirs(
        &self,
        host_id: &str,
        path: String,
    ) -> Result<civet_proto::HostDirListing, CmdError> {
        self.check_daemon_version(host_id, civet_proto::LIST_DIRS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::ListDirs { path }).await?, Response::Dirs(l) => l)
    }

    /// `host_stats`: the host's resource use and its recent history.
    pub async fn host_stats(
        &self,
        host_id: &str,
        since: Option<u64>,
        processes: bool,
    ) -> Result<civet_proto::HostStats, CmdError> {
        self.check_daemon_version(host_id, civet_proto::HOST_STATS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::HostStats { since, processes }).await?, Response::HostStats(s) => s)
    }

    /// `list_files`: one directory level of a worktree.
    pub async fn list_files(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        dir: String,
        include_ignored: bool,
    ) -> Result<civet_proto::DirListing, CmdError> {
        self.check_daemon_version(host_id, civet_proto::LIST_FILES_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::ListFiles { worktree, dir, include_ignored }).await?, Response::Files(l) => l)
    }

    /// `read_file`: a size-capped, read-only preview.
    pub async fn read_file(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        path: String,
        max_bytes: u64,
    ) -> Result<civet_proto::FilePreview, CmdError> {
        self.check_daemon_version(host_id, civet_proto::LIST_FILES_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::ReadFile { worktree, path, max_bytes }).await?, Response::FilePreview(p) => p)
    }

    /// `list_all_files`: every file of a worktree, for quick-open.
    pub async fn list_all_files(
        &self,
        host_id: &str,
        worktree: WorktreeId,
    ) -> Result<civet_proto::FileList, CmdError> {
        self.check_daemon_version(host_id, civet_proto::LIST_FILES_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::ListAllFiles { worktree }).await?, Response::AllFiles(l) => l)
    }

    /// `file_op`: create, move, copy, trash or restore worktree files.
    /// Contents are written with `upload_to_worktree` instead.
    pub async fn file_op(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        op: civet_proto::FileOp,
    ) -> Result<civet_proto::FileOpResult, CmdError> {
        if matches!(op, civet_proto::FileOp::Write { .. }) {
            return Err(CmdError::new(
                "invalidArgument",
                "file_op: use upload_to_worktree to write files",
            ));
        }
        self.request_file_op(host_id, worktree, op).await
    }

    async fn request_file_op(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        op: civet_proto::FileOp,
    ) -> Result<civet_proto::FileOpResult, CmdError> {
        self.check_daemon_version(host_id, civet_proto::FILE_OPS_MIN_DAEMON_VERSION)?;
        expect_response!(self.request(host_id, Request::FileOp { worktree, op }).await?, Response::FileOpDone(r) => r)
    }

    /// `upload_to_worktree`: copy local files and folders (dropped from
    /// Finder) into `dir` of a worktree, under their own names. Nothing is
    /// overwritten; files over [`civet_proto::MAX_UPLOAD_BYTES`] fail. One
    /// result per input path, in order (`path`: relative to the worktree;
    /// for a folder, the first failure inside it is its error).
    pub async fn upload_to_worktree(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        dir: String,
        paths: Vec<String>,
    ) -> Result<Vec<DroppedFile>, CmdError> {
        self.check_daemon_version(host_id, civet_proto::FILE_OPS_MIN_DAEMON_VERSION)?;
        let mut out = Vec::with_capacity(paths.len());
        for source in paths {
            let name = Path::new(&source)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let rel = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            let r = self.upload_tree(host_id, &worktree, &source, &rel).await;
            out.push(match r {
                Ok(()) => DroppedFile {
                    source,
                    path: Some(rel),
                    error: None,
                },
                Err(e) => DroppedFile {
                    source,
                    path: None,
                    error: Some(e.message),
                },
            });
        }
        Ok(out)
    }

    /// One dropped file or folder (recursively; `.git`, symlinks and
    /// special files left out). Stops at the first failure.
    async fn upload_tree(
        &self,
        host_id: &str,
        worktree: &WorktreeId,
        source: &str,
        rel: &str,
    ) -> Result<(), CmdError> {
        let io = |p: &Path, e: std::io::Error| CmdError::new("io", format!("{}: {e}", p.display()));
        // Depth first, parents before their children.
        let mut todo = vec![(std::path::PathBuf::from(source), rel.to_owned())];
        while let Some((local, rel)) = todo.pop() {
            let meta = tokio::fs::symlink_metadata(&local)
                .await
                .map_err(|e| io(&local, e))?;
            let op = if meta.is_dir() {
                let mut rd = tokio::fs::read_dir(&local)
                    .await
                    .map_err(|e| io(&local, e))?;
                let mut children = Vec::new();
                while let Some(e) = rd.next_entry().await.map_err(|e| io(&local, e))? {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.eq_ignore_ascii_case(".git") {
                        children.push((e.path(), format!("{rel}/{name}")));
                    }
                }
                children.sort_unstable_by(|a, b| b.1.cmp(&a.1));
                todo.extend(children);
                civet_proto::FileOp::CreateDir { path: rel }
            } else if meta.is_file() {
                if meta.len() > civet_proto::MAX_UPLOAD_BYTES as u64 {
                    return Err(CmdError::new(
                        "invalidArgument",
                        format!(
                            "{}: larger than {} MiB",
                            local.display(),
                            civet_proto::MAX_UPLOAD_BYTES >> 20
                        ),
                    ));
                }
                let data = tokio::fs::read(&local).await.map_err(|e| io(&local, e))?;
                civet_proto::FileOp::Write { path: rel, data }
            } else {
                continue;
            };
            self.request_file_op(host_id, worktree.clone(), op).await?;
        }
        Ok(())
    }

    /// `download_file`: save a worktree file to `dest` on this Mac (chosen
    /// in a save dialog, so it may be replaced), in
    /// [`civet_proto::MAX_FILE_CHUNK_BYTES`] slices. The data goes to
    /// a fresh `<dest>.<unique>.civet-part` first and replaces `dest` only once complete, so
    /// a failed download leaves `dest` as it was. Returns the bytes written.
    pub async fn download_file(
        &self,
        host_id: &str,
        worktree: WorktreeId,
        path: String,
        dest: String,
    ) -> Result<u64, CmdError> {
        use tokio::io::AsyncWriteExt;
        self.check_daemon_version(host_id, civet_proto::FILE_OPS_MIN_DAEMON_VERSION)?;
        let io = |e: std::io::Error| CmdError::new("io", format!("{dest}: {e}"));
        // Unique and exclusive: concurrent downloads to one `dest` never
        // share (or truncate) each other's part file.
        let part = format!(
            "{dest}.{}-{}.civet-part",
            std::process::id(),
            self.inner.nonce.fetch_add(1, Ordering::Relaxed)
        );
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
            .map_err(io)?;
        let mut offset = 0u64;
        let r: Result<u64, CmdError> = async {
            loop {
                let request = Request::ReadFileChunk {
                    worktree: worktree.clone(),
                    path: path.clone(),
                    offset,
                    len: civet_proto::MAX_FILE_CHUNK_BYTES,
                };
                let chunk = expect_response!(self.request(host_id, request).await?, Response::FileChunk(c) => c)?;
                if chunk.data.is_empty() {
                    break;
                }
                file.write_all(&chunk.data).await.map_err(io)?;
                offset += chunk.data.len() as u64;
                if offset >= chunk.size {
                    break;
                }
            }
            file.flush().await.map_err(io)?;
            file.sync_all().await.map_err(io)?;
            tokio::fs::rename(&part, &dest).await.map_err(io)?;
            Ok(offset)
        }
        .await;
        if r.is_err() {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
        }
        r
    }

    /// `list_panes` (also refreshes the pane cache used for notifications).
    pub async fn list_panes(&self, host_id: &str) -> Result<Vec<PaneInfo>, CmdError> {
        let host = self.host(host_id)?;
        let list = expect_response!(host.session()?.request(Request::ListPanes).await?, Response::Panes(p) => p)?;
        *lock(&host.panes) = list.iter().map(|p| (p.id, p.clone())).collect();
        Ok(list)
    }

    /// `spawn_pane`
    pub async fn spawn_pane(
        &self,
        host_id: &str,
        req: SpawnPaneRequest,
    ) -> Result<PaneInfo, CmdError> {
        let host = self.host(host_id)?;
        let info = expect_response!(host.session()?.request(Request::SpawnPane(req)).await?, Response::PaneSpawned(p) => p)?;
        lock(&host.panes).insert(info.id, info.clone());
        Ok(info)
    }

    /// `launch_agent`
    pub async fn launch_agent(
        &self,
        host_id: &str,
        req: LaunchAgentRequest,
    ) -> Result<LaunchedAgent, CmdError> {
        let host = self.host(host_id)?;
        let launched = expect_response!(
            host.session()?.request(Request::LaunchAgent(req)).await?,
            Response::AgentLaunched { worktree, pane } => LaunchedAgent { worktree, pane }
        )?;
        lock(&host.panes).insert(launched.pane.id, launched.pane.clone());
        Ok(launched)
    }

    /// `attach_pane`: stream the pane to `sink`. The first record is a Full
    /// (from the mirror for a cached view, else the daemon's). Re-attaching
    /// replaces the previous sink.
    pub async fn attach_pane(
        &self,
        host_id: &str,
        pane: PaneId,
        sink: Arc<dyn RecordSink>,
    ) -> Result<SubscribeInfo, CmdError> {
        let host = self.host(host_id)?;
        let session = host.session()?;
        // Serialized with a lingering detach's unsubscribe of this pane.
        let sub = host.sub_lock(pane);
        let _guard = sub.lock().await;
        let view = {
            let size = lock(&host.panes)
                .get(&pane)
                .map_or(TermSize::default(), |p| p.size);
            lock(&host.views)
                .entry(pane)
                .or_insert_with(|| Arc::new(Mutex::new(PaneView::new(pane, size, &self.inner.rt))))
                .clone()
        };
        if let Some(info) = lock(&view).attach(sink) {
            return Ok(info);
        }
        match session.subscribe(pane).await {
            Ok(info) => {
                lock(&view).set_subscribed(true);
                Ok(info)
            }
            Err(e) => {
                let mut views = lock(&host.views);
                if views.get(&pane).is_some_and(|v| Arc::ptr_eq(v, &view)) {
                    views.remove(&pane);
                }
                drop(views);
                lock(&view).detach();
                Err(e.into())
            }
        }
    }

    /// `detach_pane`: stop sending to the UI; keep mirroring for
    /// `detach_linger`, then unsubscribe.
    pub fn detach_pane(&self, host_id: &str, pane: PaneId) -> Result<(), CmdError> {
        let host = self.host(host_id)?;
        let Some(view) = host.view(pane) else {
            return Ok(());
        };
        let generation = lock(&view).detach();
        self.schedule_linger(host, pane, view, generation);
        Ok(())
    }

    fn schedule_linger(&self, host: Arc<Host>, pane: PaneId, view: ViewRef, generation: u64) {
        let linger = self.inner.cfg.detach_linger;
        self.inner.rt.spawn(async move {
            tokio::time::sleep(linger).await;
            // Holding the pane's subscription lock: an attach that starts
            // now waits for the unsubscribe below and then subscribes again
            // (instead of having its fresh subscription cancelled by it).
            let sub = host.sub_lock(pane);
            let _guard = sub.lock().await;
            {
                // Re-checked under the view map lock: still this view, and
                // nobody attached it since the detach.
                let mut views = lock(&host.views);
                let expired = views.get(&pane).is_some_and(|v| Arc::ptr_eq(v, &view)) && {
                    let v = lock(&view);
                    !v.is_attached() && v.attach_gen() == generation
                };
                if !expired {
                    return;
                }
                views.remove(&pane);
            }
            let session = lock(&host.session).clone();
            if let Some(s) = session {
                let _ = s.unsubscribe(pane).await;
            }
        });
    }

    /// The webview (re)loaded: every channel is dead. Detach all views; they
    /// stay cached for an instant re-attach.
    pub fn detach_all(&self) {
        let hosts: Vec<Arc<Host>> = lock(&self.inner.hosts).values().cloned().collect();
        for host in hosts {
            let views: Vec<(PaneId, ViewRef)> = lock(&host.views)
                .iter()
                .map(|(p, v)| (*p, v.clone()))
                .collect();
            for (pane, view) in views {
                let generation = {
                    let mut v = lock(&view);
                    if !v.is_attached() {
                        continue;
                    }
                    v.detach()
                };
                self.schedule_linger(host.clone(), pane, view, generation);
            }
        }
    }

    fn show_predictions(&self, host: &Host) -> bool {
        self.inner.predict.load(Ordering::Relaxed)
            && lock(&host.status).rtt_ms.is_some_and(|r| {
                Duration::from_millis(u64::from(r)) > self.inner.cfg.predict_min_rtt
            })
    }

    /// `write_input` / `write_input_binary`: numbered input (never blocks on
    /// the network), plus local-echo prediction.
    pub fn write_input(&self, host_id: &str, pane: PaneId, data: Vec<u8>) -> Result<u64, CmdError> {
        let host = self.host(host_id)?;
        let session = host.session()?;
        if data.is_empty() {
            return Ok(0);
        }
        match host.view(pane) {
            Some(view) => {
                let show = self.show_predictions(&host);
                let mut v = lock(&view);
                // Under the view lock so the overlay is ordered with the
                // screen updates of the same pane.
                let seq = session.send_input(pane, data.clone());
                v.on_input(seq, &data, show);
                Ok(seq)
            }
            None => Ok(session.send_input(pane, data)),
        }
    }

    /// `resize_pane`. Always results in a record carrying the size.
    pub async fn resize_pane(
        &self,
        host_id: &str,
        pane: PaneId,
        size: TermSize,
    ) -> Result<(), CmdError> {
        let host = self.host(host_id)?;
        let size = size.sanitized();
        expect_response!(host.session()?.request(Request::ResizePane { pane, size }).await?, Response::Ok => ())?;
        if let Some(v) = host.view(pane) {
            lock(&v).after_resize(size);
        }
        Ok(())
    }

    /// `kill_pane` (`notFound` for a pane that is already gone).
    pub async fn kill_pane(
        &self,
        host_id: &str,
        pane: PaneId,
        signal: Option<KillSignal>,
    ) -> Result<(), CmdError> {
        let signal = signal.unwrap_or_default();
        expect_response!(self.request(host_id, Request::KillPane { pane, signal }).await?, Response::Ok => ())
    }

    /// `fetch_scrollback`
    pub async fn fetch_scrollback(
        &self,
        host_id: &str,
        pane: PaneId,
        query: ScrollbackQuery,
    ) -> Result<ScrollbackPageView, CmdError> {
        let page = self.session(host_id)?.fetch_scrollback(pane, query).await?;
        let (lines, wrapped) = page
            .lines
            .iter()
            .map(|l| (String::from_utf8_lossy(&l.vt).into_owned(), l.wrapped))
            .unzip();
        Ok(ScrollbackPageView {
            start: page.start,
            oldest_available: page.oldest_available,
            end_available: page.end_available,
            lines,
            wrapped,
        })
    }

    /// `set_agent_status`
    pub async fn set_agent_status(
        &self,
        host_id: &str,
        pane: PaneId,
        status: AgentStatus,
    ) -> Result<(), CmdError> {
        expect_response!(self.request(host_id, Request::SetAgentStatus { pane, status }).await?, Response::Ok => ())
    }

    /// `set_prediction`
    pub fn set_prediction(&self, enabled: bool) {
        self.inner.predict.store(enabled, Ordering::Relaxed);
        if enabled {
            return;
        }
        let hosts: Vec<Arc<Host>> = lock(&self.inner.hosts).values().cloned().collect();
        for host in hosts {
            let views: Vec<ViewRef> = lock(&host.views).values().cloned().collect();
            for v in views {
                lock(&v).clear_predictions();
            }
        }
    }

    /// Prediction currently enabled.
    pub fn prediction_enabled(&self) -> bool {
        self.inner.predict.load(Ordering::Relaxed)
    }

    /// Run a closure on a pane view (tests, diagnostics).
    pub fn with_view<T>(
        &self,
        host_id: &str,
        pane: PaneId,
        f: impl FnOnce(&PaneView) -> T,
    ) -> Option<T> {
        let v = self.host(host_id).ok()?.view(pane)?;
        let g = lock(&v);
        Some(f(&g))
    }
}

async fn sleep_until_opt(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// One entry of a `prepare_file_drop` result.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    /// The dropped (local) path.
    pub source: String,
    /// Path to paste into the pane, on the pane's host.
    pub path: Option<String>,
    /// Why the file could not be made available.
    pub error: Option<String>,
}

/// True if semver-ish `a` is older than `b` (numeric components; anything
/// unparsable counts as 0).
fn version_older(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    parts(a) < parts(b)
}
