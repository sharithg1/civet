//! Per-connection wiring: hello / resume, control RPC → `SessionManager`,
//! screen streams per subscription, input, acks, scrollback, events.
//!
//! The same code serves QUIC connections and stream links (unix socket /
//! `civetd stdio-tunnel`); only `civet-transport` knows the difference.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use civet_proto::{
    ClientId, ClientMessage, ControlRequest, ControlResponse, DaemonInfo, ErrorCode, Event,
    HelloReject, InputMessage, PROTOCOL_VERSION, PaneId, RejectReason, Request, Response,
    ResumeToken, RpcError, ScreenAck, ServerHello, ServerMessage, SessionKey, now_ms,
};
use civet_session::{InputOutcome, PaneOutput, SessionManager, Subscription};
use civet_transport::{ServerConnection, ServerIncoming, ServerSender, close_code};
use tokio::sync::{broadcast, oneshot};
use tokio::task::AbortHandle;

/// How long a disconnected client's resume state (subscriptions, retained
/// snapshots, applied input seqs) is kept.
pub const RESUME_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long shutdown waits for a client to take its `Goodbye`.
const GOODBYE_WAIT: Duration = Duration::from_millis(1500);

/// Daemon-wide state shared by every connection.
pub(crate) struct Server {
    pub sessions: SessionManager,
    pub key: SessionKey,
    pub instance_id: u64,
    pub started_at_ms: u64,
    pub hostname: String,
    pub resume_ttl: Duration,
    clients: Mutex<HashMap<ClientId, ClientState>>,
    shutting_down: AtomicBool,
    next_conn: AtomicU64,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("instance_id", &self.instance_id)
            .field("clients", &self.lock().len())
            .finish_non_exhaustive()
    }
}

/// Per-client state; survives disconnects for `resume_ttl`.
struct ClientState {
    token: ResumeToken,
    prev_token: Option<ResumeToken>,
    /// Id of the newest connection that was granted this state.
    conn_id: u64,
    subs: HashMap<PaneId, Subscription>,
    conn: Option<ConnSlot>,
    disconnected_at: Option<Instant>,
}

/// The live connection of a client.
struct ConnSlot {
    id: u64,
    sender: ServerSender,
    /// Dropping a stop sender ends that pane's screen task.
    screen_tasks: HashMap<PaneId, oneshot::Sender<()>>,
    event_task: AbortHandle,
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.event_task.abort();
    }
}

/// What a request handler needs to know about its connection.
#[derive(Clone)]
struct ConnCtx {
    client: ClientId,
    conn_id: u64,
    sender: ServerSender,
}

impl Server {
    pub fn new(
        sessions: SessionManager,
        key: SessionKey,
        instance_id: u64,
        hostname: String,
    ) -> Self {
        Self {
            sessions,
            key,
            instance_id,
            started_at_ms: now_ms(),
            hostname,
            resume_ttl: RESUME_TTL,
            clients: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            next_conn: AtomicU64::new(1),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<ClientId, ClientState>> {
        self.clients.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    pub fn daemon_info(&self) -> DaemonInfo {
        DaemonInfo {
            daemon_version: crate::VERSION.into(),
            proto_version: PROTOCOL_VERSION,
            pid: std::process::id(),
            instance_id: self.instance_id,
            started_at_ms: self.started_at_ms,
            hostname: self.hostname.clone(),
        }
    }

    /// Serve one authenticated connection until it ends.
    pub async fn handle_connection(self: Arc<Self>, sc: ServerConnection) {
        let hello = sc.hello().clone();
        let client = hello.client_id;
        if self.is_shutting_down() {
            sc.reject(HelloReject::new(
                RejectReason::ShuttingDown,
                "daemon is shutting down",
            ))
            .await;
            return;
        }
        let token = match ResumeToken::generate() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "cannot generate resume token");
                return;
            }
        };
        // Subscribe before the hello goes out so no event is missed.
        let events = self.sessions.subscribe_events();
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let kind = sc.kind();
        let remote = sc.remote_addr();

        let (resumed, resumed_subscriptions, old_conn) = {
            let mut clients = self.lock();
            let resumable = match (clients.get(&client), &hello.resume) {
                (Some(st), Some(tok)) => {
                    st.token.ct_eq(tok) || st.prev_token.as_ref().is_some_and(|p| p.ct_eq(tok))
                }
                _ => false,
            };
            if resumable {
                let st = clients.get_mut(&client).expect("checked above");
                let old = st.conn.take();
                st.prev_token = Some(std::mem::replace(&mut st.token, token.clone()));
                st.conn_id = conn_id;
                st.disconnected_at = None;
                // Drop subscriptions of panes that are gone meanwhile.
                st.subs
                    .retain(|pane, _| self.sessions.pane_info(*pane).is_ok());
                let mut infos: Vec<_> = st.subs.values().map(Subscription::info).collect();
                infos.sort_by_key(|i| i.pane);
                (true, infos, old)
            } else {
                let old = clients.remove(&client).and_then(|mut st| st.conn.take());
                self.sessions.reset_client(client);
                clients.insert(
                    client,
                    ClientState {
                        token: token.clone(),
                        prev_token: None,
                        conn_id,
                        subs: HashMap::new(),
                        conn: None,
                        disconnected_at: None,
                    },
                );
                (false, Vec::new(), old)
            }
        };
        if let Some(old) = old_conn {
            old.sender
                .close(close_code::REPLACED, "replaced by a newer connection");
        }
        tracing::info!(
            client = %client.to_hex(),
            name = %hello.client_name,
            version = %hello.client_version,
            ?kind,
            ?remote,
            resumed,
            subs = resumed_subscriptions.len(),
            "client connected"
        );

        let server_hello = ServerHello {
            proto_version: PROTOCOL_VERSION,
            daemon_version: crate::VERSION.into(),
            instance_id: self.instance_id,
            resumed,
            server_time_ms: now_ms(),
            resume_token: token,
            resumed_subscriptions,
        };
        let (sender, mut receiver) = match sc.accept(server_hello).await {
            Ok(x) => x,
            Err(e) => {
                tracing::info!(error = %e, "connection lost during hello");
                self.mark_disconnected(client, conn_id);
                return;
            }
        };

        // Install the connection unless a newer one took over meanwhile.
        let clipboard_ok = !super::bootstrap::version_newer(
            civet_proto::CLIPBOARD_EVENT_MIN_CLIENT_VERSION,
            &hello.client_version,
        );
        let event_task = tokio::spawn(forward_events(
            events,
            sender.clone(),
            Arc::downgrade(&self),
            client,
            clipboard_ok,
        ))
        .abort_handle();
        {
            let mut clients = self.lock();
            match clients.get_mut(&client) {
                Some(st) if st.conn_id == conn_id => {
                    st.conn = Some(ConnSlot {
                        id: conn_id,
                        sender: sender.clone(),
                        screen_tasks: HashMap::new(),
                        event_task,
                    });
                }
                _ => {
                    event_task.abort();
                    drop(clients);
                    sender.close(close_code::REPLACED, "replaced by a newer connection");
                    return;
                }
            }
        }

        let ctx = ConnCtx {
            client,
            conn_id,
            sender: sender.clone(),
        };
        loop {
            match receiver.recv().await {
                Ok(Some(ServerIncoming::Message(msg))) => match msg {
                    ClientMessage::Request(req) => self.handle_request(&ctx, req).await,
                    ClientMessage::Input(msg) => self.handle_input(&ctx, &msg).await,
                    ClientMessage::Ack(ScreenAck { pane, state }) => {
                        if let Some(sub) = self.subscription(client, pane) {
                            sub.ack(state);
                        }
                    }
                    ClientMessage::Goodbye => {
                        tracing::info!(client = %client.to_hex(), "client said goodbye");
                        self.forget_client(client, conn_id);
                        return;
                    }
                },
                Ok(Some(ServerIncoming::Scrollback { request, responder })) => {
                    let result = self
                        .sessions
                        .fetch_scrollback(&request)
                        .map_err(RpcError::from);
                    tokio::spawn(async move {
                        if let Err(e) = responder.respond(result).await {
                            tracing::debug!(error = %e, "scrollback reply failed");
                        }
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!(error = %e, "connection receive failed");
                    break;
                }
            }
        }
        tracing::info!(client = %client.to_hex(), conn_id, "client disconnected");
        self.mark_disconnected(client, conn_id);
    }

    /// The connection `conn_id` ended: stop its tasks, keep resume state.
    fn mark_disconnected(&self, client: ClientId, conn_id: u64) {
        let slot = {
            let mut clients = self.lock();
            match clients.get_mut(&client) {
                Some(st) if st.conn_id == conn_id => {
                    st.disconnected_at = Some(Instant::now());
                    st.conn.take()
                }
                _ => None,
            }
        };
        drop(slot);
    }

    /// Client said Goodbye: drop all of its state now.
    fn forget_client(&self, client: ClientId, conn_id: u64) {
        let removed = {
            let mut clients = self.lock();
            match clients.get(&client) {
                Some(st) if st.conn_id == conn_id => clients.remove(&client),
                _ => None,
            }
        };
        if removed.is_some() {
            self.sessions.reset_client(client);
        }
    }

    fn subscription(&self, client: ClientId, pane: PaneId) -> Option<Subscription> {
        self.lock()
            .get(&client)
            .and_then(|st| st.subs.get(&pane).cloned())
    }

    /// Drop expired resume state. Returns how many clients were removed.
    pub fn reap_expired(&self) -> usize {
        let now = Instant::now();
        let ttl = self.resume_ttl;
        let expired: Vec<ClientId> = self
            .lock()
            .iter()
            .filter(|(_, st)| {
                st.conn.is_none()
                    && st
                        .disconnected_at
                        .is_some_and(|t| now.saturating_duration_since(t) >= ttl)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.lock().remove(id);
            self.sessions.reset_client(*id);
            tracing::info!(client = %id.to_hex(), "resume state expired");
        }
        expired.len()
    }

    /// A pane closed: forget every subscription to it.
    pub fn pane_closed(&self, pane: PaneId) {
        let mut clients = self.lock();
        for st in clients.values_mut() {
            st.subs.remove(&pane);
            if let Some(slot) = st.conn.as_mut() {
                slot.screen_tasks.remove(&pane);
            }
        }
    }

    async fn reply(&self, ctx: &ConnCtx, id: u64, result: Result<Response, RpcError>) {
        let msg = ServerMessage::Response(ControlResponse { id, result });
        if let Err(e) = ctx.sender.send(msg).await {
            tracing::debug!(error = %e, id, "response not sent");
        }
    }

    async fn handle_input(&self, ctx: &ConnCtx, msg: &InputMessage) {
        match self.sessions.write_input(ctx.client, msg) {
            Ok(InputOutcome::Applied) => {}
            Ok(InputOutcome::Duplicate) => {
                tracing::debug!(pane = %msg.pane, seq = msg.seq, "duplicate input ignored");
                // Tell the client what is already applied so it can retire
                // its buffer even without a screen subscription.
                let seq = self.sessions.last_input_seq(ctx.client, msg.pane);
                let _ = ctx
                    .sender
                    .send(ServerMessage::InputAck {
                        pane: msg.pane,
                        seq,
                    })
                    .await;
            }
            Err(e) => {
                tracing::debug!(pane = %msg.pane, seq = msg.seq, len = msg.data.len(), error = %e, "input dropped");
            }
        }
    }

    async fn handle_request(self: &Arc<Self>, ctx: &ConnCtx, req: ControlRequest) {
        let ControlRequest { id, request } = req;
        let sessions = &self.sessions;
        let result = match request {
            Request::Ping { nonce } => Ok(Response::Pong {
                nonce,
                server_time_ms: now_ms(),
            }),
            Request::GetDaemonInfo => Ok(Response::DaemonInfo(self.daemon_info())),
            Request::ListPanes => Ok(Response::Panes(sessions.list_panes())),
            Request::SubscribePane { pane, known_state } => self
                .subscribe(ctx, pane, known_state)
                .map(Response::Subscribed),
            Request::UnsubscribePane { pane } => {
                self.unsubscribe(ctx.client, pane);
                Ok(Response::Ok)
            }
            Request::ResizePane { pane, size } => sessions
                .resize_pane(pane, size)
                .map(|()| Response::Ok)
                .map_err(RpcError::from),
            Request::KillPane { pane, signal } => sessions
                .kill_pane(pane, signal)
                .map(|()| Response::Ok)
                .map_err(RpcError::from),
            Request::SetAgentStatus { pane, status } => sessions
                .set_agent_status(pane, status)
                .map(|()| Response::Ok)
                .map_err(RpcError::from),
            Request::FetchScrollback(req) => sessions
                .fetch_scrollback(&req)
                .map(Response::Scrollback)
                .map_err(RpcError::from),
            // Slow ones (git, fork/exec) run in their own task so input and
            // acks are never stuck behind them.
            slow => {
                let this = self.clone();
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let result = this.slow_request(slow).await;
                    this.reply(&ctx, id, result).await;
                });
                return;
            }
        };
        self.reply(ctx, id, result).await;
    }

    async fn slow_request(&self, request: Request) -> Result<Response, RpcError> {
        let s = &self.sessions;
        let r = match request {
            Request::ListRepos => s.list_repos().await.map(Response::Repos),
            Request::AddRepo { path } => s.add_repo(&path).await.map(Response::Repo),
            Request::RemoveRepo { repo } => s.remove_repo(&repo).await.map(|()| Response::Ok),
            Request::ListWorktrees { repo } => {
                s.list_worktrees(&repo).await.map(Response::Worktrees)
            }
            Request::CreateWorktree {
                repo,
                branch,
                base,
                path,
            } => s
                .create_worktree(&repo, &branch, base.as_deref(), path.as_deref())
                .await
                .map(Response::Worktree),
            Request::RemoveWorktree { worktree, force } => s
                .remove_worktree(&worktree, force)
                .await
                .map(|()| Response::Ok),
            Request::SpawnPane(req) => s.spawn_pane(req).await.map(Response::PaneSpawned),
            Request::LaunchAgent(req) => s
                .launch_agent(req)
                .await
                .map(|(worktree, pane)| Response::AgentLaunched { worktree, pane }),
            other => {
                return Err(RpcError::new(
                    ErrorCode::Internal,
                    format!("request not routed: {other:?}"),
                ));
            }
        };
        r.map_err(RpcError::from)
    }

    /// Get or create the subscription, reset its baseline to `known_state`
    /// (Diff if still held, else Full) and make sure a screen task runs for
    /// it on the current connection.
    fn subscribe(
        self: &Arc<Self>,
        ctx: &ConnCtx,
        pane: PaneId,
        known_state: Option<u64>,
    ) -> Result<civet_proto::SubscribeInfo, RpcError> {
        let mut clients = self.lock();
        let st = clients
            .get_mut(&ctx.client)
            .filter(|st| st.conn_id == ctx.conn_id)
            .ok_or_else(|| RpcError::new(ErrorCode::Internal, "connection was replaced"))?;
        let sub = match st.subs.get(&pane) {
            Some(sub) => sub.clone(),
            None => {
                let sub = self
                    .sessions
                    .subscribe(ctx.client, pane)
                    .map_err(RpcError::from)?;
                st.subs.insert(pane, sub.clone());
                sub
            }
        };
        sub.reset_baseline(known_state);
        if let Some(slot) = st.conn.as_mut().filter(|s| s.id == ctx.conn_id) {
            let running = slot
                .screen_tasks
                .get(&pane)
                .is_some_and(|stop| !stop.is_closed());
            if !running {
                let (stop_tx, stop_rx) = oneshot::channel();
                slot.screen_tasks.insert(pane, stop_tx);
                tokio::spawn(screen_task(
                    Arc::downgrade(self),
                    ctx.client,
                    sub.clone(),
                    slot.sender.clone(),
                    stop_rx,
                ));
            }
        }
        Ok(sub.info())
    }

    fn unsubscribe(&self, client: ClientId, pane: PaneId) {
        let mut clients = self.lock();
        if let Some(st) = clients.get_mut(&client) {
            st.subs.remove(&pane);
            if let Some(slot) = st.conn.as_mut() {
                slot.screen_tasks.remove(&pane);
            }
        }
    }

    /// Graceful shutdown of all connections: Goodbye, then close (code 4),
    /// then wait briefly until the frames left.
    pub async fn goodbye_all(&self, reason: &str) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let senders: Vec<ServerSender> = self
            .lock()
            .values()
            .filter_map(|st| st.conn.as_ref().map(|c| c.sender.clone()))
            .collect();
        tracing::info!(clients = senders.len(), reason, "saying goodbye to clients");
        let goodbyes = senders.into_iter().map(|s| {
            let reason = reason.to_string();
            async move {
                let msg = ServerMessage::Goodbye {
                    reason: reason.clone(),
                };
                let _ = tokio::time::timeout(GOODBYE_WAIT, s.send(msg)).await;
                s.close(close_code::SHUTTING_DOWN, &reason);
                let _ = tokio::time::timeout(GOODBYE_WAIT, s.closed()).await;
            }
        });
        futures_join_all(goodbyes).await;
        self.lock().clear();
    }
}

/// Minimal join_all without pulling in the futures crate.
async fn futures_join_all<F: std::future::Future<Output = ()> + Send + 'static>(
    futs: impl Iterator<Item = F>,
) {
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    for h in handles {
        let _ = h.await;
    }
}

/// Forward daemon events to one client.
/// Push daemon events to one connection. Everything goes to every client
/// except [`Event::Clipboard`] (OSC 52), which only goes to clients that
/// can decode it (`clipboard_ok`) and are subscribed to that pane.
async fn forward_events(
    mut events: broadcast::Receiver<Event>,
    sender: ServerSender,
    server: std::sync::Weak<Server>,
    client: ClientId,
    clipboard_ok: bool,
) {
    loop {
        match events.recv().await {
            Ok(ev) => {
                if let Event::Clipboard { pane, .. } = &ev {
                    let subscribed = clipboard_ok
                        && server
                            .upgrade()
                            .is_some_and(|s| s.subscription(client, *pane).is_some());
                    if !subscribed {
                        continue;
                    }
                }
                if sender.send(ServerMessage::Event(ev)).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(lost = n, "client lagging behind daemon events");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Pump one subscription onto its screen stream until the pane closes, the
/// connection dies, or `stop` fires (unsubscribe / connection replaced).
async fn screen_task(
    server: std::sync::Weak<Server>,
    client: ClientId,
    sub: Subscription,
    sender: ServerSender,
    mut stop: oneshot::Receiver<()>,
) {
    let pane = sub.pane();
    let mut stream = tokio::select! {
        r = sender.open_screen_stream(pane) => match r {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(%pane, error = %e, "cannot open screen stream");
                return;
            }
        },
        _ = &mut stop => return,
    };
    loop {
        let out = tokio::select! {
            biased;
            _ = &mut stop => {
                stream.finish().await;
                return;
            }
            out = sub.next() => out,
        };
        match out {
            None => {
                // Pane closed.
                stream.finish().await;
                if let Some(server) = server.upgrade() {
                    let mut clients = server.lock();
                    if let Some(st) = clients.get_mut(&client) {
                        st.subs.remove(&pane);
                    }
                }
                return;
            }
            Some(PaneOutput::Screen(update)) => {
                let sent = tokio::select! {
                    r = stream.send(&update) => r,
                    _ = &mut stop => return,
                };
                if let Err(e) = sent {
                    tracing::debug!(%pane, error = %e, "screen stream closed");
                    return;
                }
            }
            Some(PaneOutput::InputAck { pane, seq }) => {
                if sender
                    .send(ServerMessage::InputAck { pane, seq })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}
