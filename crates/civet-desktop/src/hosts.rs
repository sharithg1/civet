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
    AgentStatus, AgentStatusEvent, Event, KillSignal, LaunchAgentRequest, PaneId, PaneInfo,
    Request, Response, ScrollbackQuery, SpawnPaneRequest, SubscribeInfo, TermSize, WorktreeId,
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
    AgentAttentionEvent, BootstrapProgressEvent, DaemonEventPayload, HostState, HostStateEvent,
    HostSummary, HostTarget, LaunchedAgent, PingResult, ProgressStep, ScrollbackPageView,
    TransportChoice, UiEvents,
};
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
}

/// Bootstraps the daemon and dials the first connection (ssh + QUIC with
/// stdio-tunnel fallback in the app; an in-process daemon in tests).
pub trait Connector: Send + Sync + 'static {
    /// Connect; report steps through `progress`.
    fn connect<'a>(
        &'a self,
        target: &'a HostTarget,
        choice: TransportChoice,
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
    /// At most one native notification per pane per this interval.
    pub notify_interval: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            reconnecting_after: RECONNECTING_AFTER,
            detach_linger: Duration::from_secs(30),
            predict_min_rtt: DEFAULT_MIN_RTT,
            notify_interval: Duration::from_secs(10),
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
    notified: Mutex<HashMap<PaneId, Instant>>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Events of a freshly installed session, until the host task takes them.
    pending_events: Mutex<Option<UnboundedReceiver<SessionEvent>>>,
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
        }
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
    ($resp:expr, $pat:pat => $val:expr) => {
        match $resp {
            $pat => Ok($val),
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
        {
            let mut st = lock(&host.status);
            st.state = u.state;
            st.transport = u.transport.or(match u.state {
                HostState::Disconnected | HostState::Failed => None,
                _ => st.transport,
            });
            st.rtt_ms = u.rtt_ms;
        }
        self.inner.ui.host_state(HostStateEvent {
            host_id: host.id.clone(),
            state: u.state,
            transport: u.transport,
            rtt_ms: u.rtt_ms,
            message: u.message,
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
        let host = Arc::new(Host {
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
            }),
            views: Mutex::new(HashMap::new()),
            panes: Mutex::new(HashMap::new()),
            notified: Mutex::new(HashMap::new()),
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
            .connect(&target, choice, self.progress_for(&id))
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
        }
        *lock(&host.session) = Some(est.session.clone());
        *lock(&host.pending_events) = Some(est.events);
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
        let fut = self
            .inner
            .connector
            .connect(&host.target, choice, self.progress_for(&host.id));
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
                match session.subscribe(pane).await {
                    Ok(_) => lock(&view).set_subscribed(true),
                    Err(_) => {
                        lock(&view).close();
                        lock(&host.views).remove(&pane);
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
                lock(&host.notified).remove(pane);
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
            Event::Bell { .. } | Event::ReposChanged | Event::WorktreesChanged { .. } => {}
        }
        self.inner.ui.daemon_event(DaemonEventPayload {
            host_id: host.id.clone(),
            event,
        });
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
            if n.get(&e.pane)
                .is_some_and(|t| now.duration_since(*t) < self.inner.cfg.notify_interval)
            {
                return;
            }
            n.insert(e.pane, now);
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
    pub async fn daemon_info(&self, host_id: &str) -> Result<civet_proto::DaemonInfo, CmdError> {
        expect_response!(self.request(host_id, Request::GetDaemonInfo).await?, Response::DaemonInfo(i) => i)
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

    /// `list_repos`
    pub async fn list_repos(&self, host_id: &str) -> Result<Vec<civet_proto::RepoInfo>, CmdError> {
        expect_response!(self.request(host_id, Request::ListRepos).await?, Response::Repos(r) => r)
    }

    /// `add_repo`
    pub async fn add_repo(
        &self,
        host_id: &str,
        path: String,
    ) -> Result<civet_proto::RepoInfo, CmdError> {
        expect_response!(self.request(host_id, Request::AddRepo { path }).await?, Response::Repo(r) => r)
    }

    /// `remove_repo`
    pub async fn remove_repo(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
    ) -> Result<(), CmdError> {
        expect_response!(self.request(host_id, Request::RemoveRepo { repo }).await?, Response::Ok => ())
    }

    /// `list_worktrees`
    pub async fn list_worktrees(
        &self,
        host_id: &str,
        repo: civet_proto::RepoId,
    ) -> Result<Vec<civet_proto::WorktreeInfo>, CmdError> {
        expect_response!(self.request(host_id, Request::ListWorktrees { repo }).await?, Response::Worktrees(w) => w)
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
            let expired = {
                let v = lock(&view);
                !v.is_attached() && v.attach_gen() == generation
            };
            if !expired {
                return;
            }
            {
                let mut views = lock(&host.views);
                if !views.get(&pane).is_some_and(|v| Arc::ptr_eq(v, &view)) {
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
