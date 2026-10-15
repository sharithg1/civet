//! Client-side resumable session: owns the current connection, reconnects on
//! its own, and hides migrations and resumes from the caller.
//!
//! One supervisor task per session reads the current connection, runs the
//! heartbeat / stall detector, and drives (re)dials. Caller-facing methods
//! only touch shared state under a short-lived mutex and enqueue frames on
//! the connection's unbounded writer, so they never wait on the network
//! (except for awaiting RPC responses).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use std::collections::HashSet;

use civet_proto::{
    ClientHello, ClientId, ClientMessage, ControlRequest, ControlResponse, ErrorCode, Event,
    InputMessage, PaneId, Request, RequestId, Response, RpcError, ScreenAck, ScreenUpdate,
    ScrollbackPage, ScrollbackQuery, ScrollbackRequest, ServerHello, ServerMessage, SessionKey,
    SubscribeInfo, UpdateKind, now_ms,
};
use futures::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::conn::{ClientConnection, ClientReceiver, Connected, TransportKind};
use crate::error::TransportError;
use crate::quic::{QuicClient, QuicClientConfig};

/// How to (re)open a link. Implemented for QUIC and for stream links.
pub trait Dialer: Send + Sync + 'static {
    /// Open a new connection and perform the hello (see
    /// `QuicClient::connect` / `connect_stream`).
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>>;
    /// Called on network change; QUIC rebinds its endpoint. Default: Ok(()).
    fn rebind(&self) -> std::io::Result<()> {
        Ok(())
    }
    /// Link kind this dialer produces (for a failover dialer: the
    /// preferred kind).
    fn kind(&self) -> TransportKind;
    /// Hint from the session: the link has been in trouble (stalled / lost)
    /// since `since`, or recovered (`None`). A failover dialer uses it to
    /// decide when to try its fallback. Default: ignored.
    fn link_trouble(&self, since: Option<std::time::Instant>) {
        let _ = since;
    }
    /// While the session is connected over a link whose kind differs from
    /// [`Dialer::kind`] (i.e. on a fallback), it calls
    /// [`Dialer::dial_preferred`] this often to move back. None (default):
    /// never.
    fn upgrade_interval(&self) -> Option<Duration> {
        None
    }
    /// Dial only the preferred link (used to move back from a fallback).
    /// Default: [`Dialer::dial`].
    fn dial_preferred(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        self.dial(hello, pipelined)
    }
}

/// `LinkState::Failed::reason` when the daemon closed the connection because
/// another connection with the same `ClientId` took over (twice within
/// [`REPLACED_WINDOW`]); see [`TransportError::Replaced`].
pub const REPLACED_REASON: &str =
    "taken over by another connection with the same client id (close code 3)";

/// After a first "replaced" close the session redials once after this delay
/// (the replacing connection may have been a stale dial of its own).
pub const REPLACED_RETRY_DELAY: Duration = Duration::from_secs(1);

/// A second "replaced" close within this window ends the session with
/// `LinkState::Failed { reason: REPLACED_REASON, needs_bootstrap: false }`
/// instead of kicking the other connection off in turn.
pub const REPLACED_WINDOW: Duration = Duration::from_secs(30);

/// Unacked input that saw no ack progress for this long is sent again on
/// the same connection (the daemon dedups by seq). Covers input the daemon
/// refused under PTY backpressure, which it neither applies nor acks.
pub const INPUT_RETRANSMIT_AFTER: Duration = Duration::from_secs(1);

/// At most this many bytes of a pane's unacked input are retransmitted per
/// [`INPUT_RETRANSMIT_AFTER`] period.
const INPUT_RETRANSMIT_BYTES: usize = 64 * 1024;

/// Consecutive connections lost sooner than this after they were
/// established escalate the reconnect backoff (no instant redial loops).
const SHORT_LIVED: Duration = Duration::from_millis(500);

/// Dials QUIC connections on a shared endpoint.
#[derive(Debug)]
pub struct QuicDialer {
    /// Endpoint (rebinding it migrates every connection on it).
    pub client: Arc<QuicClient>,
    /// Daemon address and pinned fingerprint.
    pub config: QuicClientConfig,
}

impl Dialer for QuicDialer {
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        Box::pin(self.client.connect(&self.config, hello, pipelined))
    }

    fn rebind(&self) -> std::io::Result<()> {
        self.client.rebind()
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Quic
    }
}

/// Dials by calling a factory that returns a fresh byte stream (e.g. spawns
/// `ssh host civetd stdio-tunnel`).
pub struct StreamDialer<F> {
    /// Produces a new connected byte stream per dial.
    pub factory: F,
}

impl<F> fmt::Debug for StreamDialer<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamDialer")
    }
}

impl<F, Fut, S> Dialer for StreamDialer<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = std::io::Result<S>> + Send + 'static,
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        let fut = (self.factory)();
        Box::pin(async move {
            let io = fut.await?;
            crate::stream::connect_stream(io, hello, pipelined).await
        })
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Stream
    }
}

/// Session tuning and identity.
#[derive(Debug, Clone)]
pub struct ClientSessionConfig {
    /// From the bootstrap line.
    pub session_key: SessionKey,
    /// Stable per installation.
    pub client_id: ClientId,
    /// "civet-desktop" | "civet-cli"
    pub client_name: String,
    /// env!("CARGO_PKG_VERSION")
    pub client_version: String,
    /// Send a Ping when nothing was received for this long. Default 1 s.
    pub heartbeat: Duration,
    /// A ping unanswered for this long => `LinkState::Recovering`. Default 1 s.
    pub stall_after: Duration,
    /// While Recovering for this long, dial a fresh connection in parallel
    /// (with the resume token) and keep whichever answers first. Default 3 s.
    pub race_after: Duration,
    /// Reconnect backoff schedule after a connection is lost; the last value
    /// repeats forever. Default [0, 100, 250, 500, 1000, 2000, 5000] ms.
    pub backoff: Vec<Duration>,
}

impl ClientSessionConfig {
    /// Config with the documented default timings.
    pub fn new(
        session_key: SessionKey,
        client_id: ClientId,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self {
            session_key,
            client_id,
            client_name: client_name.into(),
            client_version: client_version.into(),
            heartbeat: Duration::from_secs(1),
            stall_after: Duration::from_secs(1),
            race_after: Duration::from_secs(3),
            backoff: [0, 100, 250, 500, 1000, 2000, 5000]
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
        }
    }

    fn backoff_at(&self, attempt: usize) -> Duration {
        match self.backoff.len() {
            0 => Duration::ZERO,
            n => self.backoff[attempt.min(n - 1)],
        }
    }
}

/// Link health as seen by the session.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LinkState {
    /// First connect in progress.
    Connecting,
    /// Healthy.
    Connected {
        /// Link kind.
        transport: TransportKind,
        /// Latest RTT estimate.
        rtt_ms: Option<u32>,
    },
    /// Something is wrong (stall, lost connection, network change) and
    /// recovery is running. `since_ms` = civet_proto::now_ms() when it began.
    /// The UI shows "reconnecting" only if this lasts > 1 s (desktop's job).
    Recovering {
        /// When recovery began (ms since epoch).
        since_ms: u64,
        /// Why.
        reason: String,
    },
    /// Fatal: needs re-bootstrap (BadKey / instance gone / version mismatch)
    /// or the user closed it.
    Failed {
        /// Why.
        reason: String,
        /// Re-run bootstrap over SSH before reconnecting.
        needs_bootstrap: bool,
    },
    /// Closed by the caller.
    Closed,
}

/// What the session reports to its owner.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Validated, in-order screen update for a subscribed pane. For a `Diff`,
    /// `base_state` equalled the pane's current client state. The session
    /// has already queued the `ScreenAck` for it.
    Screen(ScreenUpdate),
    /// Highest input seq the daemon applied for this pane (from
    /// `ServerMessage::InputAck` or `ScreenUpdate::acked_input_seq`).
    InputAcked {
        /// Pane.
        pane: PaneId,
        /// Highest applied seq.
        seq: u64,
    },
    /// Pushed daemon event.
    Event(Event),
    /// Link state change.
    Link(LinkState),
    /// A (re)connection completed. `resumed=false` after the first connect or
    /// when resume failed. `instance_changed` => daemon restarted: all panes
    /// are gone; the caller should drop its pane views.
    Connected {
        /// The daemon restored the previous session.
        resumed: bool,
        /// The daemon restarted since the last connection.
        instance_changed: bool,
        /// The daemon's hello.
        server_hello: ServerHello,
    },
    /// Daemon said Goodbye (upgrade/shutdown).
    Goodbye {
        /// Daemon's reason.
        reason: String,
    },
}

/// Why a session request failed.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// The daemon answered with an error.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// The connection died before the response arrived. Requests are not
    /// retried automatically (they may not be idempotent).
    #[error("connection lost before response")]
    ConnectionLost,
    /// The session was closed (or failed permanently).
    #[error("session closed")]
    Closed,
    /// The daemon answered with the wrong response variant.
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PaneState {
    /// Last applied server state (0 = nothing applied).
    current_state: u64,
    next_input_seq: u64,
    unacked_input: VecDeque<(u64, Vec<u8>)>,
    subscribed: bool,
    /// A Diff did not match; a forced Full was requested.
    awaiting_full: bool,
    /// Highest input seq reported as applied.
    acked_input: u64,
    /// Last time unacked input was (re)sent or its ack advanced.
    input_progress: Instant,
}

impl PaneState {
    fn new() -> Self {
        Self {
            current_state: 0,
            next_input_seq: 1,
            unacked_input: VecDeque::new(),
            subscribed: false,
            awaiting_full: false,
            acked_input: 0,
            input_progress: Instant::now(),
        }
    }

    fn drop_acked(&mut self, seq: u64) {
        while self.unacked_input.front().is_some_and(|(s, _)| *s <= seq) {
            self.unacked_input.pop_front();
        }
    }
}

type SubWaiter = oneshot::Sender<Result<SubscribeInfo, RequestError>>;

enum Pending {
    Request {
        generation: u64,
        tx: oneshot::Sender<Result<Response, RequestError>>,
    },
    Subscribe {
        generation: u64,
        pane: PaneId,
        /// Re-subscription pipelined with a reconnect (the pane was live
        /// before the link dropped).
        resubscribe: bool,
    },
    Ping {
        generation: u64,
        sent: Instant,
    },
}

impl Pending {
    fn generation(&self) -> u64 {
        match self {
            Pending::Request { generation, .. }
            | Pending::Subscribe { generation, .. }
            | Pending::Ping { generation, .. } => *generation,
        }
    }
}

struct Shared {
    conn: Option<ClientConnection>,
    /// Generation of `conn`.
    generation: u64,
    next_generation: u64,
    /// Closed or failed permanently.
    finished: bool,
    next_id: RequestId,
    pending: HashMap<RequestId, Pending>,
    sub_waiters: HashMap<PaneId, Vec<SubWaiter>>,
    panes: HashMap<PaneId, PaneState>,
    /// Panes known to be gone (PaneClosed, or re-subscribe said NotFound):
    /// input for them is not buffered.
    closed_panes: HashSet<PaneId>,
    server_hello: ServerHello,
    ping_rtt: Option<Duration>,
}

impl Shared {
    fn alloc_id(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Send on the current connection; errors are ignored (the supervisor
    /// notices a dead link and recovers).
    fn send(&self, msg: &ClientMessage) -> bool {
        match &self.conn {
            Some(c) => c.send_now(msg).is_ok(),
            None => false,
        }
    }

    /// Remove pending entries matching `pred`, failing request waiters.
    fn fail_pending(&mut self, pred: impl Fn(u64) -> bool, err: fn() -> RequestError) {
        let ids: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, p)| pred(p.generation()))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(Pending::Request { tx, .. }) = self.pending.remove(&id) {
                let _ = tx.send(Err(err()));
            }
        }
    }

    fn resolve_waiters(
        &mut self,
        pane: PaneId,
        result: impl Fn() -> Result<SubscribeInfo, RequestError>,
    ) {
        if let Some(ws) = self.sub_waiters.remove(&pane) {
            for w in ws {
                let _ = w.send(result());
            }
        }
    }

    fn fail_all(&mut self) {
        self.fail_pending(|_| true, || RequestError::Closed);
        self.pending.clear();
        for (_, ws) in self.sub_waiters.drain() {
            for w in ws {
                let _ = w.send(Err(RequestError::Closed));
            }
        }
    }

    fn send_subscribe(&mut self, pane: PaneId, known_state: Option<u64>) {
        if self.conn.is_none() {
            return;
        }
        let id = self.alloc_id();
        self.pending.insert(
            id,
            Pending::Subscribe {
                generation: self.generation,
                pane,
                resubscribe: false,
            },
        );
        self.send(&ClientMessage::Request(ControlRequest {
            id,
            request: Request::SubscribePane { pane, known_state },
        }));
    }
}

struct Inner {
    config: ClientSessionConfig,
    dialer: Arc<dyn Dialer>,
    state: Mutex<Shared>,
    events: mpsc::UnboundedSender<SessionEvent>,
    link: watch::Sender<LinkState>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        // A panic while holding the lock leaves consistent-enough state.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, ev: SessionEvent) {
        let _ = self.events.send(ev);
    }

    fn set_link(&self, s: LinkState) {
        let changed = self.link.send_if_modified(|cur| {
            if *cur != s {
                *cur = s.clone();
                true
            } else {
                false
            }
        });
        if changed {
            self.emit(SessionEvent::Link(s));
        }
    }
}

enum Cmd {
    NetworkChange,
    Close(oneshot::Sender<()>),
}

struct Handle {
    inner: Arc<Inner>,
    cmd: mpsc::UnboundedSender<Cmd>,
}

/// Clone = same session.
#[derive(Clone)]
pub struct ClientSession {
    handle: Arc<Handle>,
}

impl fmt::Debug for ClientSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientSession")
            .field("client_name", &self.handle.inner.config.client_name)
            .field("link", &*self.handle.inner.link.borrow())
            .finish()
    }
}

impl ClientSession {
    /// First connect: dial once (no retries) so the caller can fall back to
    /// another Dialer (QUIC -> SSH stdio) on error. Then spawns the
    /// supervisor task. Events go to the returned receiver (unbounded; screen
    /// updates are never dropped).
    pub async fn connect(
        config: ClientSessionConfig,
        dialer: Arc<dyn Dialer>,
    ) -> Result<(ClientSession, mpsc::UnboundedReceiver<SessionEvent>), TransportError> {
        let hello = ClientHello::new(
            config.session_key.clone(),
            config.client_id,
            config.client_name.clone(),
            config.client_version.clone(),
        );
        let connected = dialer.dial(hello, Vec::new()).await?;
        let (events, events_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            config,
            dialer,
            state: Mutex::new(Shared {
                conn: None,
                generation: 0,
                next_generation: 1,
                finished: false,
                next_id: 1,
                pending: HashMap::new(),
                sub_waiters: HashMap::new(),
                panes: HashMap::new(),
                closed_panes: HashSet::new(),
                server_hello: connected.server_hello.clone(),
                ping_rtt: None,
            }),
            events,
            link: watch::channel(LinkState::Connecting).0,
        });
        let mut sup = Supervisor::new(inner.clone(), cmd_rx);
        sup.dial_generation = 0;
        sup.adopt(connected, true);
        tokio::spawn(sup.run());
        Ok((
            ClientSession {
                handle: Arc::new(Handle { inner, cmd: cmd_tx }),
            },
            events_rx,
        ))
    }

    fn inner(&self) -> &Inner {
        &self.handle.inner
    }

    /// Watch the link state.
    pub fn link_state(&self) -> watch::Receiver<LinkState> {
        self.inner().link.subscribe()
    }

    /// The latest server hello.
    pub fn server_hello(&self) -> ServerHello {
        self.inner().lock().server_hello.clone()
    }

    /// Generic RPC; prefer the helpers below for subscribe/scrollback.
    pub async fn request(&self, request: Request) -> Result<Response, RequestError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.inner().lock();
            if st.finished {
                return Err(RequestError::Closed);
            }
            if st.conn.is_none() {
                return Err(RequestError::ConnectionLost);
            }
            let id = st.alloc_id();
            let generation = st.generation;
            st.pending.insert(id, Pending::Request { generation, tx });
            if !st.send(&ClientMessage::Request(ControlRequest { id, request })) {
                st.pending.remove(&id);
                return Err(RequestError::ConnectionLost);
            }
        }
        rx.await.unwrap_or(Err(RequestError::Closed))
    }

    /// Subscribe (or reset to Full if already subscribed). The session
    /// remembers subscriptions and re-subscribes after every reconnect with
    /// `known_state = current client state`.
    pub async fn subscribe(&self, pane: PaneId) -> Result<SubscribeInfo, RequestError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut st = self.inner().lock();
            if st.finished {
                return Err(RequestError::Closed);
            }
            let p = st.panes.entry(pane).or_insert_with(PaneState::new);
            p.subscribed = true;
            p.awaiting_full = false;
            st.sub_waiters.entry(pane).or_default().push(tx);
            // Without a connection the next (re)connect pipelines it.
            st.send_subscribe(pane, None);
        }
        rx.await.unwrap_or(Err(RequestError::Closed))
    }

    /// Stop screen updates for a pane.
    pub async fn unsubscribe(&self, pane: PaneId) -> Result<(), RequestError> {
        {
            let mut st = self.inner().lock();
            if let Some(p) = st.panes.get_mut(&pane) {
                p.subscribed = false;
                p.awaiting_full = false;
            }
            st.resolve_waiters(pane, || {
                Err(RequestError::Unexpected("unsubscribed".into()))
            });
        }
        match self.request(Request::UnsubscribePane { pane }).await? {
            Response::Ok => Ok(()),
            other => Err(RequestError::Unexpected(format!("{other:?}"))),
        }
    }

    /// Assign the next seq for this pane, buffer the bytes until acked, and
    /// send. Never blocks on the network; returns the seq, or 0 (nothing
    /// sent or buffered) if the pane is known to be closed.
    pub fn send_input(&self, pane: PaneId, data: Vec<u8>) -> u64 {
        let mut st = self.inner().lock();
        if st.closed_panes.contains(&pane) {
            return 0;
        }
        let p = st.panes.entry(pane).or_insert_with(PaneState::new);
        let seq = p.next_input_seq;
        p.next_input_seq += 1;
        if p.unacked_input.is_empty() {
            p.input_progress = Instant::now();
        }
        p.unacked_input.push_back((seq, data.clone()));
        if !st.finished {
            st.send(&ClientMessage::Input(InputMessage { pane, seq, data }));
        }
        seq
    }

    /// Fetch a page of history.
    pub async fn fetch_scrollback(
        &self,
        pane: PaneId,
        query: ScrollbackQuery,
    ) -> Result<ScrollbackPage, RequestError> {
        let (conn, req_id) = {
            let mut st = self.inner().lock();
            if st.finished {
                return Err(RequestError::Closed);
            }
            let conn = st.conn.clone().ok_or(RequestError::ConnectionLost)?;
            (conn, st.alloc_id())
        };
        match conn
            .fetch_scrollback(ScrollbackRequest {
                req_id,
                pane,
                query,
            })
            .await
        {
            Ok(Ok(page)) => Ok(page),
            Ok(Err(rpc)) => Err(RequestError::Rpc(rpc)),
            Err(_) => Err(RequestError::ConnectionLost),
        }
    }

    fn current_conn(&self) -> Result<ClientConnection, RequestError> {
        let st = self.inner().lock();
        if st.finished {
            return Err(RequestError::Closed);
        }
        st.conn.clone().ok_or(RequestError::ConnectionLost)
    }

    /// Listening-port updates from the daemon, on the current connection.
    /// The watch ends with the connection; open a new one after the next
    /// `SessionEvent::Connected`. `Rpc(Unsupported)` on stream links.
    pub async fn watch_ports(&self) -> Result<crate::PortsWatch, RequestError> {
        match self.current_conn()?.watch_ports().await {
            Ok(Ok(w)) => Ok(w),
            Ok(Err(rpc)) => Err(RequestError::Rpc(rpc)),
            Err(_) => Err(RequestError::ConnectionLost),
        }
    }

    /// Forward one TCP connection to `port` on the daemon's loopback
    /// interface, on the current connection. `Rpc(NotFound)`: nothing
    /// listens there; `Rpc(Unsupported)`: stream link or an older daemon.
    pub async fn open_forward(&self, port: u16) -> Result<crate::Tunnel, RequestError> {
        match self.current_conn()?.open_forward(port).await {
            Ok(Ok(t)) => Ok(t),
            Ok(Err(rpc)) => Err(RequestError::Rpc(rpc)),
            Err(_) => Err(RequestError::ConnectionLost),
        }
    }

    /// Hint from the OS/UI: wake from sleep or network change. Rebinds the
    /// QUIC endpoint, sends a Ping immediately, and starts stall detection.
    /// This is what makes "the next packet is the reconnect" true.
    pub fn notify_network_change(&self) {
        let _ = self.handle.cmd.send(Cmd::NetworkChange);
    }

    /// Goodbye + close; LinkState::Closed.
    pub async fn close(&self) {
        let (tx, rx) = oneshot::channel();
        if self.handle.cmd.send(Cmd::Close(tx)).is_ok() {
            let _ = rx.await;
        }
        let inner = self.inner();
        inner.lock().finished = true;
        inner.set_link(LinkState::Closed);
    }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

type DialFuture = Pin<Box<dyn Future<Output = Result<Connected, TransportError>> + Send>>;

struct Supervisor {
    inner: Arc<Inner>,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    rx: Option<ClientReceiver>,
    last_rx: Instant,
    /// Outstanding heartbeat ping (id, sent).
    ping_out: Option<(RequestId, Instant)>,
    recovering_since: Option<Instant>,
    race_at: Option<Instant>,
    dial: Option<DialFuture>,
    dial_generation: u64,
    attempt: usize,
    next_dial_at: Option<Instant>,
    goodbye: Option<String>,
    stopped: bool,
    /// When the current connection was adopted.
    adopted_at: Instant,
    /// Consecutive connections lost within [`SHORT_LIVED`].
    short_lived_losses: usize,
    /// Last "replaced" close (see [`REPLACED_WINDOW`]).
    last_replaced: Option<Instant>,
    /// Next attempt to move back to the preferred link (while on a fallback).
    upgrade_at: Option<Instant>,
    /// The dial in flight is such an upgrade attempt.
    dial_upgrade: bool,
    /// Next check for stale unacked input.
    input_check_at: Instant,
}

async fn recv_opt(
    rx: &mut Option<ClientReceiver>,
) -> Result<Option<ServerMessage>, TransportError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

async fn dial_opt(dial: &mut Option<DialFuture>) -> Result<Connected, TransportError> {
    match dial {
        Some(d) => d.await,
        None => std::future::pending().await,
    }
}

impl Supervisor {
    fn new(inner: Arc<Inner>, cmd_rx: mpsc::UnboundedReceiver<Cmd>) -> Self {
        Self {
            inner,
            cmd_rx,
            rx: None,
            last_rx: Instant::now(),
            ping_out: None,
            recovering_since: None,
            race_at: None,
            dial: None,
            dial_generation: 0,
            attempt: 0,
            next_dial_at: None,
            goodbye: None,
            stopped: false,
            adopted_at: Instant::now(),
            short_lived_losses: 0,
            last_replaced: None,
            upgrade_at: None,
            dial_upgrade: false,
            input_check_at: Instant::now() + INPUT_RETRANSMIT_AFTER,
        }
    }

    async fn run(mut self) {
        while !self.stopped {
            let deadline = self.deadline();
            tokio::select! {
                r = recv_opt(&mut self.rx) => self.on_rx(r),
                r = dial_opt(&mut self.dial) => {
                    self.dial = None;
                    self.on_dial(r);
                }
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(Cmd::NetworkChange) => self.on_network_change(),
                    Some(Cmd::Close(done)) => {
                        self.close_gracefully().await;
                        let _ = done.send(());
                    }
                    None => self.drop_session(),
                },
                _ = tokio::time::sleep_until(deadline) => self.on_timer(),
            }
        }
    }

    fn deadline(&self) -> Instant {
        let far = Instant::now() + Duration::from_secs(86_400);
        let cfg = &self.inner.config;
        if self.rx.is_some() {
            let upgrade = match (self.recovering_since, &self.dial) {
                (None, None) => self.upgrade_at.unwrap_or(far),
                _ => far,
            };
            let upgrade = match self.recovering_since {
                None => upgrade.min(self.input_check_at),
                Some(_) => upgrade,
            };
            let t = match (self.recovering_since, self.ping_out) {
                (None, Some((_, sent))) => sent + cfg.stall_after,
                (None, None) => self.last_rx + cfg.heartbeat,
                (Some(_), _) if self.dial.is_none() => self.race_at.unwrap_or(far),
                _ => far,
            };
            t.min(upgrade)
        } else if self.dial.is_none() {
            self.next_dial_at.unwrap_or(far)
        } else {
            far
        }
    }

    fn connected_state(&self) -> LinkState {
        let st = self.inner.lock();
        let rtt = st
            .conn
            .as_ref()
            .and_then(|c| c.rtt())
            .or(st.ping_rtt)
            .map(|d| d.as_millis().min(u32::MAX as u128) as u32);
        LinkState::Connected {
            transport: st
                .conn
                .as_ref()
                .map_or_else(|| self.inner.dialer.kind(), |c| c.kind()),
            rtt_ms: rtt,
        }
    }

    fn enter_recovering(&mut self, reason: &str) {
        if self.recovering_since.is_none() {
            let now = Instant::now();
            self.recovering_since = Some(now);
            self.inner.dialer.link_trouble(Some(now.into_std()));
            tracing::info!(reason, "link recovering");
            self.inner.set_link(LinkState::Recovering {
                since_ms: now_ms(),
                reason: reason.to_string(),
            });
        }
    }

    fn send_ping(&mut self) {
        let mut st = self.inner.lock();
        if st.conn.is_none() {
            return;
        }
        let id = st.alloc_id();
        let now = Instant::now();
        let generation = st.generation;
        st.pending.insert(
            id,
            Pending::Ping {
                generation,
                sent: now,
            },
        );
        st.send(&ClientMessage::Request(ControlRequest {
            id,
            request: Request::Ping { nonce: id },
        }));
        self.ping_out = Some((id, now));
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        let cfg = self.inner.config.clone();
        if self.rx.is_some() {
            if self.recovering_since.is_none() && now >= self.input_check_at {
                self.input_check_at = now + INPUT_RETRANSMIT_AFTER;
                self.retransmit_stale_input(now);
            }
            if self.recovering_since.is_none()
                && self.dial.is_none()
                && self.upgrade_at.is_some_and(|t| now >= t)
            {
                self.upgrade_at = None;
                tracing::debug!("on a fallback link; trying the preferred one");
                self.start_dial_kind(true);
                return;
            }
            if self.recovering_since.is_none() {
                match self.ping_out {
                    Some((_, sent)) if now >= sent + cfg.stall_after => {
                        self.enter_recovering("no response from daemon");
                        if let Err(e) = self.inner.dialer.rebind() {
                            tracing::warn!(error = %e, "rebind failed");
                        }
                        self.send_ping();
                        self.race_at = Some(now + cfg.race_after);
                    }
                    None if now >= self.last_rx + cfg.heartbeat => self.send_ping(),
                    _ => {}
                }
            } else if self.dial.is_none() && self.race_at.is_some_and(|t| now >= t) {
                tracing::info!("still stalled; racing a second connection");
                self.start_dial();
            }
        } else if self.dial.is_none() && self.next_dial_at.is_some_and(|t| now >= t) {
            self.start_dial();
        }
    }

    fn on_network_change(&mut self) {
        if let Err(e) = self.inner.dialer.rebind() {
            tracing::warn!(error = %e, "rebind after network change failed");
        }
        if self.rx.is_some() {
            self.send_ping();
        } else if self.dial.is_none() {
            self.attempt = 0;
            self.next_dial_at = Some(Instant::now());
        }
    }

    fn on_rx(&mut self, r: Result<Option<ServerMessage>, TransportError>) {
        match r {
            Ok(Some(msg)) => {
                self.last_rx = Instant::now();
                if self.recovering_since.is_some() {
                    self.recovered_in_place();
                }
                self.handle_message(msg);
            }
            Ok(None) => self.on_conn_lost("connection closed by daemon".into()),
            // Kicked off by another connection with our client id. If a dial
            // of our own is in flight, that dial is the newer connection:
            // just a normal loss.
            Err(TransportError::Replaced(why)) if self.dial.is_none() => {
                let now = Instant::now();
                if self
                    .last_replaced
                    .is_some_and(|t| now.saturating_duration_since(t) < REPLACED_WINDOW)
                {
                    tracing::warn!(%why, "connection taken over again; giving up");
                    self.fail(REPLACED_REASON.to_string(), false);
                    return;
                }
                tracing::warn!(%why, "connection taken over by another connection of this client");
                self.last_replaced = Some(now);
                self.on_conn_lost(format!("replaced: {why}"));
                self.next_dial_at = Some(now + REPLACED_RETRY_DELAY);
            }
            Err(e) => self.on_conn_lost(e.to_string()),
        }
    }

    /// Resend (a bounded prefix of) unacked input that made no progress for
    /// [`INPUT_RETRANSMIT_AFTER`]. Safe: the daemon applies a seq at most
    /// once and answers duplicates with an `InputAck`.
    fn retransmit_stale_input(&mut self, now: Instant) {
        let mut st = self.inner.lock();
        if st.conn.is_none() {
            return;
        }
        let mut resend = Vec::new();
        for (pane, p) in st.panes.iter_mut() {
            if p.unacked_input.is_empty()
                || now.saturating_duration_since(p.input_progress) < INPUT_RETRANSMIT_AFTER
            {
                continue;
            }
            p.input_progress = now;
            let mut bytes = 0;
            for (seq, data) in &p.unacked_input {
                if bytes > 0 && bytes + data.len() > INPUT_RETRANSMIT_BYTES {
                    break;
                }
                bytes += data.len();
                resend.push(InputMessage {
                    pane: *pane,
                    seq: *seq,
                    data: data.clone(),
                });
            }
        }
        if !resend.is_empty() {
            tracing::debug!(n = resend.len(), "retransmitting unacked input");
        }
        for m in resend {
            st.send(&ClientMessage::Input(m));
        }
    }

    /// The stalled connection delivered something: cancel any race dial.
    fn recovered_in_place(&mut self) {
        self.recovering_since = None;
        self.race_at = None;
        self.ping_out = None;
        if self.dial.take().is_some() {
            let g = self.dial_generation;
            self.inner.lock().pending.retain(|_, p| p.generation() != g);
        }
        self.inner.dialer.link_trouble(None);
        self.schedule_upgrade();
        let s = self.connected_state();
        self.inner.set_link(s);
    }

    /// While connected over a non-preferred link, plan the next attempt to
    /// move back (see [`Dialer::upgrade_interval`]).
    fn schedule_upgrade(&mut self) {
        let on_fallback = self
            .inner
            .lock()
            .conn
            .as_ref()
            .is_some_and(|c| c.kind() != self.inner.dialer.kind());
        self.upgrade_at = match self.inner.dialer.upgrade_interval() {
            Some(every) if on_fallback => Some(Instant::now() + every),
            _ => None,
        };
    }

    fn handle_message(&mut self, msg: ServerMessage) {
        let inner = self.inner.clone();
        let mut st = inner.lock();
        match msg {
            ServerMessage::Response(ControlResponse { id, result }) => match st.pending.remove(&id)
            {
                Some(Pending::Request { tx, .. }) => {
                    let _ = tx.send(result.map_err(RequestError::Rpc));
                }
                Some(Pending::Subscribe {
                    pane, resubscribe, ..
                }) => match result {
                    Ok(Response::Subscribed(info)) => {
                        st.closed_panes.remove(&pane);
                        if let Some(p) = st.panes.get_mut(&pane) {
                            p.drop_acked(info.last_input_seq);
                            if p.next_input_seq <= info.last_input_seq {
                                p.next_input_seq = info.last_input_seq + 1;
                            }
                        }
                        st.resolve_waiters(pane, || Ok(info));
                    }
                    Ok(other) => {
                        let msg = format!("{other:?}");
                        st.resolve_waiters(pane, || Err(RequestError::Unexpected(msg.clone())));
                    }
                    Err(rpc)
                        if rpc.code == ErrorCode::NotFound
                            && (resubscribe
                                || st.panes.get(&pane).is_some_and(|p| p.current_state > 0)) =>
                    {
                        // A pane we were showing is gone (typically closed
                        // while we were disconnected, so its PaneClosed
                        // event was never delivered): forget it, stop
                        // buffering its input and tell the caller.
                        tracing::debug!(%pane, "re-subscribe: pane no longer exists");
                        st.panes.remove(&pane);
                        st.closed_panes.insert(pane);
                        st.resolve_waiters(pane, || Err(RequestError::Rpc(rpc.clone())));
                        inner.emit(SessionEvent::Event(Event::PaneClosed { pane }));
                    }
                    Err(rpc) => {
                        if let Some(p) = st.panes.get_mut(&pane) {
                            p.subscribed = false;
                        }
                        st.resolve_waiters(pane, || Err(RequestError::Rpc(rpc.clone())));
                    }
                },
                Some(Pending::Ping { sent, .. }) => {
                    let rtt = Instant::now() - sent;
                    let prev = st.ping_rtt.replace(rtt);
                    if self.ping_out.is_some_and(|(pid, _)| pid == id) {
                        self.ping_out = None;
                    }
                    drop(st);
                    // Refresh the reported RTT when it moved noticeably.
                    let moved = prev.is_none_or(|p| {
                        let (a, b) = (p.as_millis() as i64, rtt.as_millis() as i64);
                        (a - b).abs() > 10 && (a - b).abs() * 4 > a.max(b)
                    });
                    if moved && self.recovering_since.is_none() {
                        let s = self.connected_state();
                        inner.set_link(s);
                    }
                }
                None => {}
            },
            ServerMessage::Event(ev) => {
                if let Event::PaneCreated(info) = &ev {
                    st.closed_panes.remove(&info.id);
                }
                if let Event::PaneClosed { pane } = &ev {
                    let pane = *pane;
                    st.panes.remove(&pane);
                    st.closed_panes.insert(pane);
                    st.resolve_waiters(pane, || Err(RequestError::Rpc(RpcError::not_found(pane))));
                }
                inner.emit(SessionEvent::Event(ev));
            }
            ServerMessage::Screen(update) => Self::apply_screen(&inner, &mut st, update),
            ServerMessage::InputAck { pane, seq } => {
                Self::apply_input_ack(&inner, &mut st, pane, seq)
            }
            ServerMessage::Goodbye { reason } => {
                tracing::info!(%reason, "daemon said goodbye");
                self.goodbye = Some(reason.clone());
                inner.emit(SessionEvent::Goodbye { reason });
            }
        }
    }

    fn apply_screen(inner: &Inner, st: &mut Shared, u: ScreenUpdate) {
        let pane = u.pane;
        let Some(p) = st.panes.get_mut(&pane) else {
            return;
        };
        if !p.subscribed {
            return;
        }
        let applies = match u.kind {
            UpdateKind::Full => true,
            UpdateKind::Diff => u.base_state == p.current_state,
        };
        if !applies {
            if !p.awaiting_full {
                tracing::debug!(%pane, base = u.base_state, have = p.current_state, "diff base mismatch; forcing full");
                p.awaiting_full = true;
                st.send_subscribe(pane, None);
            }
            return;
        }
        if u.kind == UpdateKind::Full {
            p.awaiting_full = false;
        }
        p.current_state = u.state;
        let state = u.state;
        let acked = u.acked_input_seq;
        st.send(&ClientMessage::Ack(ScreenAck { pane, state }));
        inner.emit(SessionEvent::Screen(u));
        Self::apply_input_ack(inner, st, pane, acked);
    }

    fn apply_input_ack(inner: &Inner, st: &mut Shared, pane: PaneId, seq: u64) {
        let Some(p) = st.panes.get_mut(&pane) else {
            return;
        };
        p.drop_acked(seq);
        if seq > p.acked_input {
            p.acked_input = seq;
            p.input_progress = Instant::now();
            inner.emit(SessionEvent::InputAcked { pane, seq });
        }
    }

    fn on_conn_lost(&mut self, reason: String) {
        tracing::info!(%reason, "connection lost");
        self.rx = None;
        self.ping_out = None;
        {
            let mut st = self.inner.lock();
            if let Some(c) = st.conn.take() {
                c.abort("lost");
            }
            let g = st.generation;
            st.fail_pending(|pg| pg == g, || RequestError::ConnectionLost);
            st.pending.retain(|_, p| p.generation() != g);
        }
        if let Some(reason) = self.goodbye.take() {
            self.fail(format!("daemon said goodbye: {reason}"), true);
            return;
        }
        self.enter_recovering(&reason);
        self.upgrade_at = None;
        if self.dial.is_none() {
            // Connections that keep dying right after they were established
            // (e.g. kicked off by another client) escalate the backoff
            // instead of redialing at once; a single short-lived one does not.
            if self.adopted_at.elapsed() >= SHORT_LIVED {
                self.short_lived_losses = 0;
            } else {
                self.short_lived_losses += 1;
            }
            self.attempt = self.short_lived_losses.saturating_sub(1);
            self.next_dial_at = Some(Instant::now() + self.inner.config.backoff_at(self.attempt));
        }
    }

    fn start_dial(&mut self) {
        self.start_dial_kind(false);
    }

    /// Start a (re)dial; `upgrade` = only the preferred link, while the
    /// current (fallback) connection still works.
    fn start_dial_kind(&mut self, upgrade: bool) {
        let (hello, pipelined) = {
            let mut st = self.inner.lock();
            let generation = st.next_generation;
            st.next_generation += 1;
            self.dial_generation = generation;
            let cfg = &self.inner.config;
            let hello = ClientHello::new(
                cfg.session_key.clone(),
                cfg.client_id,
                cfg.client_name.clone(),
                cfg.client_version.clone(),
            )
            .with_resume(Some(st.server_hello.resume_token.clone()));
            let mut panes: Vec<(PaneId, u64)> = st
                .panes
                .iter_mut()
                .filter(|(_, p)| p.subscribed)
                .map(|(id, p)| {
                    p.awaiting_full = false;
                    (*id, p.current_state)
                })
                .collect();
            panes.sort();
            let mut pipelined = Vec::with_capacity(panes.len());
            for (pane, current) in panes {
                let id = st.alloc_id();
                st.pending.insert(
                    id,
                    Pending::Subscribe {
                        generation,
                        pane,
                        resubscribe: true,
                    },
                );
                pipelined.push(ClientMessage::Request(ControlRequest {
                    id,
                    request: Request::SubscribePane {
                        pane,
                        known_state: (current > 0).then_some(current),
                    },
                }));
            }
            (hello, pipelined)
        };
        self.next_dial_at = None;
        self.dial_upgrade = upgrade;
        let dialer = self.inner.dialer.clone();
        self.dial = Some(Box::pin(async move {
            if upgrade {
                dialer.dial_preferred(hello, pipelined).await
            } else {
                dialer.dial(hello, pipelined).await
            }
        }));
    }

    fn on_dial(&mut self, r: Result<Connected, TransportError>) {
        match r {
            Ok(c) => self.adopt(c, false),
            Err(e) => {
                let g = self.dial_generation;
                self.inner.lock().pending.retain(|_, p| p.generation() != g);
                if self.dial_upgrade && self.rx.is_some() && !e.is_fatal() {
                    // Still on the fallback; try again later.
                    tracing::debug!(error = %e, "preferred link still unavailable");
                    self.schedule_upgrade();
                    return;
                }
                if e.is_fatal() || matches!(e, TransportError::Rejected(_)) {
                    tracing::warn!(error = %e, "reconnect refused");
                    self.fail(e.to_string(), true);
                    return;
                }
                tracing::debug!(error = %e, attempt = self.attempt, "dial failed");
                let now = Instant::now();
                if self.rx.is_some() {
                    self.race_at = Some(now + self.inner.config.race_after);
                } else {
                    self.attempt += 1;
                    self.next_dial_at = Some(now + self.inner.config.backoff_at(self.attempt));
                }
            }
        }
    }

    /// Install a freshly dialed connection.
    fn adopt(&mut self, c: Connected, first: bool) {
        let Connected {
            conn,
            incoming,
            server_hello: sh,
        } = c;
        let generation = self.dial_generation;
        let inner = self.inner.clone();
        let instance_changed;
        {
            let mut st = inner.lock();
            if let Some(old) = st.conn.take() {
                old.abort("replaced by a newer connection");
            }
            st.conn = Some(conn);
            st.generation = generation;
            st.fail_pending(|g| g != generation, || RequestError::ConnectionLost);
            st.pending.retain(|_, p| p.generation() == generation);

            instance_changed = !first && sh.instance_id != st.server_hello.instance_id;
            if instance_changed {
                // Pane ids from the old daemon are void. Undo the pipelined
                // subscriptions (ids may now name unrelated panes).
                let panes: Vec<PaneId> = st.panes.keys().copied().collect();
                st.pending.clear();
                for pane in panes {
                    let id = st.alloc_id();
                    st.send(&ClientMessage::Request(ControlRequest {
                        id,
                        request: Request::UnsubscribePane { pane },
                    }));
                    st.resolve_waiters(pane, || Err(RequestError::ConnectionLost));
                }
                st.panes.clear();
                st.closed_panes.clear();
            } else if sh.resumed {
                let mut resumed: HashMap<PaneId, u64> = HashMap::new();
                for info in &sh.resumed_subscriptions {
                    resumed.insert(info.pane, info.last_input_seq);
                }
                let mut resend = Vec::new();
                let mut acked = Vec::new();
                for (pane, p) in st.panes.iter_mut() {
                    match resumed.get(pane) {
                        Some(&last) => {
                            p.drop_acked(last);
                            if p.next_input_seq <= last {
                                p.next_input_seq = last + 1;
                            }
                            if last > p.acked_input {
                                p.acked_input = last;
                                acked.push((*pane, last));
                            }
                            p.input_progress = Instant::now();
                            for (seq, data) in &p.unacked_input {
                                resend.push(InputMessage {
                                    pane: *pane,
                                    seq: *seq,
                                    data: data.clone(),
                                });
                            }
                        }
                        None if !p.unacked_input.is_empty() => {
                            // Unknown applied seq: never risk duplicates.
                            tracing::warn!(%pane, n = p.unacked_input.len(), "dropping unconfirmed input for pane without resumed subscription");
                            p.unacked_input.clear();
                        }
                        None => {}
                    }
                }
                resend.sort_by_key(|m| (m.pane, m.seq));
                if !resend.is_empty() {
                    tracing::debug!(n = resend.len(), "resending unacked input after resume");
                }
                for m in resend {
                    st.send(&ClientMessage::Input(m));
                }
                for (pane, seq) in acked {
                    inner.emit(SessionEvent::InputAcked { pane, seq });
                }
            } else {
                // Fresh session: old input may have been applied; never
                // duplicate keystrokes. Numbering restarts at 1.
                for p in st.panes.values_mut() {
                    p.unacked_input.clear();
                    p.next_input_seq = 1;
                    p.acked_input = 0;
                }
            }
            // Subscriptions requested after the dial started were not
            // pipelined: send them now so their callers get an answer.
            let missing: Vec<PaneId> = st
                .panes
                .iter()
                .filter(|(pane, p)| {
                    p.subscribed
                        && !st.pending.values().any(
                            |q| matches!(q, Pending::Subscribe { pane: qp, .. } if qp == *pane),
                        )
                })
                .map(|(pane, _)| *pane)
                .collect();
            for pane in missing {
                st.send_subscribe(pane, None);
            }
            st.server_hello = sh.clone();
            inner.emit(SessionEvent::Connected {
                resumed: sh.resumed,
                instance_changed,
                server_hello: sh,
            });
        }
        self.rx = Some(incoming);
        self.last_rx = Instant::now();
        self.ping_out = None;
        self.recovering_since = None;
        self.race_at = None;
        self.next_dial_at = None;
        self.dial = None;
        self.dial_upgrade = false;
        self.goodbye = None;
        self.adopted_at = Instant::now();
        inner.dialer.link_trouble(None);
        self.schedule_upgrade();
        let s = self.connected_state();
        inner.set_link(s);
    }

    fn fail(&mut self, reason: String, needs_bootstrap: bool) {
        {
            let mut st = self.inner.lock();
            st.finished = true;
            if let Some(c) = st.conn.take() {
                c.abort("failed");
            }
            st.fail_all();
        }
        self.rx = None;
        self.dial = None;
        self.stopped = true;
        self.inner.set_link(LinkState::Failed {
            reason,
            needs_bootstrap,
        });
    }

    async fn close_gracefully(&mut self) {
        let conn = {
            let mut st = self.inner.lock();
            st.finished = true;
            st.fail_all();
            st.conn.take()
        };
        if let Some(c) = conn {
            c.close().await;
        }
        self.rx = None;
        self.dial = None;
        self.stopped = true;
        self.inner.set_link(LinkState::Closed);
    }

    /// Every `ClientSession` handle was dropped.
    fn drop_session(&mut self) {
        {
            let mut st = self.inner.lock();
            st.finished = true;
            st.fail_all();
            if let Some(c) = st.conn.take() {
                c.abort("session dropped");
            }
        }
        self.rx = None;
        self.dial = None;
        self.stopped = true;
        self.inner.set_link(LinkState::Closed);
    }
}
