//! Transport-agnostic connection handles shared by the QUIC and byte-stream
//! links: [`ServerConnection`] / [`ServerSender`] / [`ServerReceiver`] on the
//! daemon side and [`ClientConnection`] / [`ClientReceiver`] on the client.
//!
//! Each handle wraps a private link (QUIC streams or one ordered byte stream).
//! Writes go through a per-connection writer task fed by an unbounded queue so
//! frames are never interleaved and callers are cancel-safe; reads happen in
//! dedicated reader tasks (framing reads are not cancel-safe) that feed an
//! mpsc channel.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use civet_proto::{
    ClientHello, ClientMessage, ControlRequest, ControlResponse, HelloReject, HelloReply, PaneId,
    Request, RequestId, Response, RpcError, ScreenUpdate, ScrollbackPage, ScrollbackRequest,
    ServerHello, ServerMessage, StreamOpen, encode_frame,
};
use futures::future::BoxFuture;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use crate::error::{TransportError, close_code, frame_error, stream_error};

/// Which kind of link a connection runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind {
    /// QUIC over UDP.
    Quic,
    /// Framed protocol over one ordered byte stream (SSH stdio tunnel, unix
    /// socket).
    Stream,
}

/// How long a graceful close waits for queued frames to be delivered.
const GRACEFUL_CLOSE_WAIT: Duration = Duration::from_millis(500);
/// Bound of the daemon-side incoming queue (backpressure into the link).
const SERVER_RX_QUEUE: usize = 256;
/// Request ids used internally for `FetchScrollback` on stream links. Kept in
/// the top half of the id space so they never collide with caller ids.
const INTERNAL_ID_BASE: RequestId = 1 << 63;

pub(crate) type BoxedRead = Box<dyn AsyncRead + Send + Unpin>;
pub(crate) type BoxedWrite = Box<dyn AsyncWrite + Send + Unpin>;

// ---------------------------------------------------------------------------
// Internal plumbing
// ---------------------------------------------------------------------------

/// Encode a screen update as `ServerMessage::Screen` without cloning it.
/// Postcard encodes enum variants by index, so a mirror enum whose third
/// variant is `Screen` produces identical bytes.
pub(crate) fn encode_screen_frame(update: &ScreenUpdate) -> Result<Bytes, TransportError> {
    #[derive(Serialize)]
    #[allow(dead_code)] // only the `Screen` variant is constructed
    enum ServerMessageRef<'a> {
        Response(()),
        Event(()),
        Screen(&'a ScreenUpdate),
    }
    Ok(Bytes::from(encode_frame(&ServerMessageRef::Screen(
        update,
    ))?))
}

pub(crate) fn frame<T: Serialize + ?Sized>(msg: &T) -> Result<Bytes, TransportError> {
    Ok(Bytes::from(encode_frame(msg)?))
}

/// Commands for a writer task.
pub(crate) enum WriteCmd {
    /// Write one encoded frame; signal the optional sender once written.
    Frame(Bytes, Option<oneshot::Sender<()>>),
    /// Finish the stream (FIN / shutdown), wait briefly for delivery, stop.
    Finish(oneshot::Sender<()>),
}

/// A write half the writer task can drive.
pub(crate) trait FrameSink: AsyncWrite + Unpin + Send + 'static {
    /// Resolves once data written before `shutdown` was delivered (QUIC: the
    /// peer read the stream to its end). Default: immediately.
    fn drained(&mut self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

impl FrameSink for quinn::SendStream {
    fn drained(&mut self) -> BoxFuture<'static, ()> {
        let stopped = self.stopped();
        Box::pin(async move {
            let _ = stopped.await;
        })
    }
}

impl FrameSink for BoxedWrite {}

/// Spawn the writer task for a link. Errors are reported through `closer`.
pub(crate) fn spawn_writer<W: FrameSink>(
    mut w: W,
    mut rx: mpsc::UnboundedReceiver<WriteCmd>,
    closer: Arc<Closer>,
) -> AbortHandle {
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                WriteCmd::Frame(bytes, ack) => {
                    if let Err(e) = w.write_all(&bytes).await {
                        closer.close(format!("write failed: {e}"));
                        break;
                    }
                    if rx.is_empty()
                        && let Err(e) = w.flush().await
                    {
                        closer.close(format!("flush failed: {e}"));
                        break;
                    }
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                }
                WriteCmd::Finish(done) => {
                    let _ = w.flush().await;
                    let _ = w.shutdown().await;
                    let _ = tokio::time::timeout(GRACEFUL_CLOSE_WAIT, w.drained()).await;
                    let _ = done.send(());
                    break;
                }
            }
        }
    })
    .abort_handle()
}

/// Local close / failure flag shared by a link's tasks and handles.
#[derive(Debug)]
pub(crate) struct Closer {
    tx: watch::Sender<Option<String>>,
}

impl Closer {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: watch::channel(None).0,
        })
    }

    /// Record the first close reason (later ones are ignored).
    pub(crate) fn close(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.tx.send_if_modified(|cur| {
            if cur.is_none() {
                *cur = Some(reason);
                true
            } else {
                false
            }
        });
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.tx.borrow().is_some()
    }

    pub(crate) async fn wait(&self) -> String {
        let mut rx = self.tx.subscribe();
        loop {
            if let Some(r) = rx.borrow_and_update().clone() {
                return r;
            }
            if rx.changed().await.is_err() {
                return "closed".into();
            }
        }
    }
}

/// Items flowing from reader tasks to a receiver.
pub(crate) enum RxItem<T> {
    Item(T),
    /// Clean end (peer finished the control stream / said Goodbye).
    End,
    Failed(TransportError),
}

/// Abort handles of a link's background tasks.
#[derive(Debug, Default)]
pub(crate) struct Tasks(Mutex<Vec<AbortHandle>>);

impl Tasks {
    pub(crate) fn push(&self, h: AbortHandle) {
        if let Ok(mut v) = self.0.lock() {
            v.retain(|h| !h.is_finished());
            v.push(h);
        }
    }

    pub(crate) fn abort_all(&self) {
        if let Ok(mut v) = self.0.lock() {
            for h in v.drain(..) {
                h.abort();
            }
        }
    }
}

async fn send_with_ack(
    writer: &mpsc::UnboundedSender<WriteCmd>,
    closer: &Closer,
    bytes: Bytes,
) -> Result<(), TransportError> {
    let (tx, rx) = oneshot::channel();
    if writer.send(WriteCmd::Frame(bytes, Some(tx))).is_err() {
        return Err(closed_err(closer));
    }
    rx.await.map_err(|_| closed_err(closer))
}

fn closed_err(closer: &Closer) -> TransportError {
    TransportError::Closed(
        closer
            .tx
            .borrow()
            .clone()
            .unwrap_or_else(|| "connection closed".into()),
    )
}

async fn finish_writer(writer: &mpsc::UnboundedSender<WriteCmd>) {
    let (tx, rx) = oneshot::channel();
    if writer.send(WriteCmd::Finish(tx)).is_ok() {
        let _ = tokio::time::timeout(GRACEFUL_CLOSE_WAIT * 2, rx).await;
    }
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

pub(crate) enum PendingServerLink {
    Quic {
        conn: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    },
    Stream {
        read: BoxedRead,
        write: BoxedWrite,
    },
}

/// An authenticated connection whose hello is not yet answered.
pub struct ServerConnection {
    pub(crate) hello: ClientHello,
    pub(crate) link: PendingServerLink,
}

impl fmt::Debug for ServerConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConnection")
            .field("client_id", &self.hello.client_id)
            .field("client_name", &self.hello.client_name)
            .field("kind", &self.kind())
            .finish()
    }
}

impl ServerConnection {
    /// The validated client hello.
    pub fn hello(&self) -> &ClientHello {
        &self.hello
    }

    /// Link kind.
    pub fn kind(&self) -> TransportKind {
        match self.link {
            PendingServerLink::Quic { .. } => TransportKind::Quic,
            PendingServerLink::Stream { .. } => TransportKind::Stream,
        }
    }

    /// Client address; None for stream links.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        match &self.link {
            PendingServerLink::Quic { conn, .. } => Some(conn.remote_address()),
            PendingServerLink::Stream { .. } => None,
        }
    }

    /// Send `HelloReply::Accepted(hello)` and start the I/O tasks.
    pub async fn accept(
        self,
        hello: ServerHello,
    ) -> Result<(ServerSender, ServerReceiver), TransportError> {
        let reply = frame(&HelloReply::Accepted(hello))?;
        let closer = Closer::new();
        let tasks = Arc::new(Tasks::default());
        let (wtx, wrx) = mpsc::unbounded_channel();
        let (itx, irx) = mpsc::channel(SERVER_RX_QUEUE);
        let kind = self.kind();
        let quic = match self.link {
            PendingServerLink::Quic {
                conn,
                mut send,
                recv,
            } => {
                send.write_all(&reply)
                    .await
                    .map_err(|e| stream_error(&conn, e))?;
                tasks.push(spawn_writer(send, wrx, closer.clone()));
                tasks.push(crate::quic::spawn_server_readers(
                    conn.clone(),
                    recv,
                    itx,
                    wtx.clone(),
                ));
                Some(conn)
            }
            PendingServerLink::Stream { read, mut write } => {
                write.write_all(&reply).await?;
                write.flush().await?;
                tasks.push(spawn_writer(write, wrx, closer.clone()));
                tasks.push(crate::stream::spawn_server_reader(
                    read,
                    itx,
                    wtx.clone(),
                    closer.clone(),
                ));
                None
            }
        };
        let local = Closer::new();
        let shared = Arc::new(ServerShared {
            kind,
            writer: wtx,
            quic,
            closer,
            local: local.clone(),
            tasks,
        });
        Ok((
            ServerSender {
                inner: shared.clone(),
            },
            ServerReceiver {
                rx: irx,
                done: false,
                local,
                _shared: shared,
            },
        ))
    }

    /// Send `HelloReply::Rejected` and close (code 1).
    pub async fn reject(self, reject: HelloReject) {
        match self.link {
            PendingServerLink::Quic { conn, send, .. } => {
                crate::quic::reject_quic(&conn, send, &reject).await;
            }
            PendingServerLink::Stream { write, .. } => {
                crate::stream::reject_stream(write, &reject).await;
            }
        }
    }
}

struct ServerShared {
    kind: TransportKind,
    writer: mpsc::UnboundedSender<WriteCmd>,
    quic: Option<quinn::Connection>,
    /// Link ended (either side).
    closer: Arc<Closer>,
    /// `ServerSender::close` was called: stop delivering incoming items.
    local: Arc<Closer>,
    tasks: Arc<Tasks>,
}

impl Drop for ServerShared {
    fn drop(&mut self) {
        // Last handle gone: stop the I/O tasks (drops the stream halves) and
        // close the QUIC connection.
        self.tasks.abort_all();
        if let Some(conn) = &self.quic {
            conn.close(close_code::NORMAL.into(), b"dropped");
        }
    }
}

/// Cheap to clone; all methods are usable from any task.
#[derive(Clone)]
pub struct ServerSender {
    inner: Arc<ServerShared>,
}

impl fmt::Debug for ServerSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerSender")
            .field("kind", &self.inner.kind)
            .field("remote_addr", &self.remote_addr())
            .finish()
    }
}

impl ServerSender {
    /// Link kind.
    pub fn kind(&self) -> TransportKind {
        self.inner.kind
    }

    /// Queue a message on the control stream (Response, Event, InputAck,
    /// Goodbye). Messages are written in call order. Returns when the frame
    /// is accepted by the stream (backpressure), not when delivered.
    /// Passing `ServerMessage::Screen` here is allowed but bypasses per-pane
    /// streams; use `open_screen_stream`.
    pub async fn send(&self, msg: ServerMessage) -> Result<(), TransportError> {
        let bytes = frame(&msg)?;
        send_with_ack(&self.inner.writer, &self.inner.closer, bytes).await
    }

    /// Per-pane screen channel. QUIC: opens a uni stream and writes
    /// `StreamOpen::Screen { pane }`. Stream link: a handle that writes
    /// `ServerMessage::Screen` frames into the shared stream.
    pub async fn open_screen_stream(&self, pane: PaneId) -> Result<ScreenStream, TransportError> {
        match &self.inner.quic {
            Some(conn) => {
                let mut send = conn.open_uni().await.map_err(|e| stream_error(conn, e))?;
                let header = frame(&StreamOpen::Screen { pane })?;
                send.write_all(&header)
                    .await
                    .map_err(|e| stream_error(conn, e))?;
                Ok(ScreenStream {
                    pane,
                    inner: ScreenInner::Quic {
                        send,
                        conn: conn.clone(),
                    },
                })
            }
            None => Ok(ScreenStream {
                pane,
                inner: ScreenInner::Shared {
                    writer: self.inner.writer.clone(),
                    closer: self.inner.closer.clone(),
                },
            }),
        }
    }

    /// Close the connection with an application code (2/3/4, see 2.3).
    /// Already queued control frames get a short grace period to go out.
    pub fn close(&self, code: u32, reason: &str) {
        let inner = self.inner.clone();
        let reason = reason.to_string();
        inner.local.close(reason.clone());
        tokio::spawn(async move {
            finish_writer(&inner.writer).await;
            match &inner.quic {
                Some(conn) => conn.close(code.into(), reason.as_bytes()),
                None => {
                    inner.closer.close(reason);
                    inner.tasks.abort_all();
                }
            }
        });
    }

    /// Resolves when the connection is gone (either side, any reason).
    pub async fn closed(&self) -> TransportError {
        match &self.inner.quic {
            Some(conn) => crate::error::from_conn_error(&conn.closed().await),
            None => TransportError::Closed(self.inner.closer.wait().await),
        }
    }

    /// Current RTT estimate (QUIC), None for stream links.
    pub fn rtt(&self) -> Option<Duration> {
        self.inner.quic.as_ref().map(|c| c.rtt())
    }

    /// Current client address (changes on migration), None for streams.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.inner.quic.as_ref().map(|c| c.remote_address())
    }
}

enum ScreenInner {
    Quic {
        send: quinn::SendStream,
        conn: quinn::Connection,
    },
    Shared {
        writer: mpsc::UnboundedSender<WriteCmd>,
        closer: Arc<Closer>,
    },
}

/// Per-pane channel for screen updates.
pub struct ScreenStream {
    pane: PaneId,
    inner: ScreenInner,
}

impl fmt::Debug for ScreenStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScreenStream")
            .field("pane", &self.pane)
            .finish_non_exhaustive()
    }
}

impl ScreenStream {
    /// Pane this stream carries.
    pub fn pane(&self) -> PaneId {
        self.pane
    }

    /// Write one `ServerMessage::Screen(update)` frame. Awaits flow-control
    /// credit: callers rely on this to coalesce (see civet-session).
    pub async fn send(&mut self, update: &ScreenUpdate) -> Result<(), TransportError> {
        if update.pane != self.pane {
            return Err(TransportError::ProtocolViolation(format!(
                "screen update for {} on stream of {}",
                update.pane, self.pane
            )));
        }
        let bytes = encode_screen_frame(update)?;
        match &mut self.inner {
            ScreenInner::Quic { send, conn } => send
                .write_all(&bytes)
                .await
                .map_err(|e| stream_error(conn, e)),
            ScreenInner::Shared { writer, closer } => send_with_ack(writer, closer, bytes).await,
        }
    }

    /// Finish the stream (QUIC FIN); no-op on stream links.
    pub async fn finish(self) {
        if let ScreenInner::Quic { mut send, .. } = self.inner {
            let _ = send.finish();
        }
    }
}

/// An item received by the daemon.
#[derive(Debug)]
pub enum ServerIncoming {
    /// Anything on the control stream except `Request::FetchScrollback`.
    Message(ClientMessage),
    /// From a Scrollback stream (QUIC) or a `Request::FetchScrollback` on the
    /// control stream (normalized here so the daemon has one code path).
    Scrollback {
        /// The request.
        request: ScrollbackRequest,
        /// Where to send the answer.
        responder: ScrollbackResponder,
    },
}

/// Receiving half of a daemon-side connection.
pub struct ServerReceiver {
    rx: mpsc::Receiver<RxItem<ServerIncoming>>,
    done: bool,
    local: Arc<Closer>,
    _shared: Arc<ServerShared>,
}

impl fmt::Debug for ServerReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerReceiver")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl ServerReceiver {
    /// Next incoming item. Ok(None) after a clean close / Goodbye.
    /// Cancel-safe (backed by an mpsc fed by reader tasks).
    pub async fn recv(&mut self) -> Result<Option<ServerIncoming>, TransportError> {
        if self.done {
            return Ok(None);
        }
        // Once closed locally, stop delivering (e.g. replaced connection).
        if self.local.is_closed() {
            self.done = true;
            return Ok(None);
        }
        let item = tokio::select! {
            biased;
            _ = self.local.wait() => None,
            item = self.rx.recv() => item,
        };
        match item {
            Some(RxItem::Item(i)) => Ok(Some(i)),
            Some(RxItem::Failed(e)) => {
                self.done = true;
                Err(e)
            }
            Some(RxItem::End) | None => {
                self.done = true;
                Ok(None)
            }
        }
    }
}

pub(crate) enum ResponderInner {
    Quic {
        send: quinn::SendStream,
        conn: quinn::Connection,
    },
    Control {
        id: RequestId,
        writer: mpsc::UnboundedSender<WriteCmd>,
    },
}

/// Answers one scrollback request.
pub struct ScrollbackResponder {
    pub(crate) inner: ResponderInner,
}

impl fmt::Debug for ScrollbackResponder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            ResponderInner::Quic { .. } => f.write_str("ScrollbackResponder(quic stream)"),
            ResponderInner::Control { id, .. } => {
                write!(f, "ScrollbackResponder(control id {id})")
            }
        }
    }
}

impl ScrollbackResponder {
    /// QUIC: write the result on the scrollback stream and finish it.
    /// Stream link: send `ControlResponse { id, result: .map(Response::Scrollback) }`.
    pub async fn respond(
        self,
        result: Result<ScrollbackPage, RpcError>,
    ) -> Result<(), TransportError> {
        match self.inner {
            ResponderInner::Quic { mut send, conn } => {
                let bytes = frame(&result)?;
                send.write_all(&bytes)
                    .await
                    .map_err(|e| stream_error(&conn, e))?;
                send.finish().map_err(|e| stream_error(&conn, e))?;
                Ok(())
            }
            ResponderInner::Control { id, writer } => {
                let msg = ServerMessage::Response(ControlResponse {
                    id,
                    result: result.map(Response::Scrollback),
                });
                let (tx, rx) = oneshot::channel();
                writer
                    .send(WriteCmd::Frame(frame(&msg)?, Some(tx)))
                    .map_err(|_| TransportError::Closed("connection closed".into()))?;
                rx.await
                    .map_err(|_| TransportError::Closed("connection closed".into()))
            }
        }
    }
}

/// Turn a decoded client control message into a `ServerIncoming`.
pub(crate) fn classify_client_message(
    msg: ClientMessage,
    writer: &mpsc::UnboundedSender<WriteCmd>,
) -> ServerIncoming {
    match msg {
        ClientMessage::Request(ControlRequest {
            id,
            request: Request::FetchScrollback(request),
        }) => ServerIncoming::Scrollback {
            request,
            responder: ScrollbackResponder {
                inner: ResponderInner::Control {
                    id,
                    writer: writer.clone(),
                },
            },
        },
        other => ServerIncoming::Message(other),
    }
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Result of a successful client handshake.
#[derive(Debug)]
pub struct Connected {
    /// Send side / control handle.
    pub conn: ClientConnection,
    /// Everything the daemon sends.
    pub incoming: ClientReceiver,
    /// The daemon's hello.
    pub server_hello: ServerHello,
}

/// Routes `Response::Scrollback` on stream links back to `fetch_scrollback`.
#[derive(Debug, Default)]
pub(crate) struct ScrollRouter {
    next: AtomicU64,
    waiters: Mutex<HashMap<RequestId, oneshot::Sender<Result<Response, RpcError>>>>,
}

impl ScrollRouter {
    /// If `resp` answers an internal request, hand it over and return None.
    pub(crate) fn route(&self, resp: ControlResponse) -> Option<ControlResponse> {
        if resp.id < INTERNAL_ID_BASE {
            return Some(resp);
        }
        let waiter = self.waiters.lock().ok()?.remove(&resp.id);
        match waiter {
            Some(tx) => {
                let _ = tx.send(resp.result);
                None
            }
            None => Some(resp),
        }
    }

    /// Fail all waiters (link gone).
    pub(crate) fn clear(&self) {
        if let Ok(mut w) = self.waiters.lock() {
            w.clear();
        }
    }
}

pub(crate) struct ClientShared {
    pub(crate) kind: TransportKind,
    pub(crate) writer: mpsc::UnboundedSender<WriteCmd>,
    pub(crate) closer: Arc<Closer>,
    pub(crate) quic: Option<(quinn::Connection, quinn::Endpoint)>,
    pub(crate) scroll: Arc<ScrollRouter>,
    pub(crate) tasks: Arc<Tasks>,
}

impl Drop for ClientShared {
    fn drop(&mut self) {
        // Last handle gone: no Goodbye, so the daemon keeps resume state.
        self.tasks.abort_all();
        if let Some((conn, _)) = &self.quic {
            conn.close(close_code::NORMAL.into(), b"dropped");
        }
    }
}

/// Transport-agnostic client connection handle. Clone = same connection.
#[derive(Clone)]
pub struct ClientConnection {
    pub(crate) inner: Arc<ClientShared>,
}

impl fmt::Debug for ClientConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("kind", &self.inner.kind)
            .field("closed", &self.inner.closer.is_closed())
            .finish()
    }
}

impl ClientConnection {
    /// Link kind.
    pub fn kind(&self) -> TransportKind {
        self.inner.kind
    }

    /// Queue a message on the control stream, in call order. The client-side
    /// queue is unbounded (client traffic is small), so this never waits for
    /// the network.
    pub async fn send(&self, msg: ClientMessage) -> Result<(), TransportError> {
        self.send_now(&msg)
    }

    /// Synchronous form of [`send`](Self::send).
    pub(crate) fn send_now(&self, msg: &ClientMessage) -> Result<(), TransportError> {
        if self.inner.closer.is_closed() {
            return Err(closed_err(&self.inner.closer));
        }
        let bytes = frame(msg)?;
        self.inner
            .writer
            .send(WriteCmd::Frame(bytes, None))
            .map_err(|_| closed_err(&self.inner.closer))
    }

    /// QUIC: open a Scrollback bidi stream. Stream link: send
    /// `Request::FetchScrollback` and wait for the matching response.
    pub async fn fetch_scrollback(
        &self,
        req: ScrollbackRequest,
    ) -> Result<Result<ScrollbackPage, RpcError>, TransportError> {
        match &self.inner.quic {
            Some((conn, _)) => crate::quic::fetch_scrollback(conn, req).await,
            None => {
                let router = &self.inner.scroll;
                let id = INTERNAL_ID_BASE + router.next.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = oneshot::channel();
                if let Ok(mut w) = router.waiters.lock() {
                    w.insert(id, tx);
                }
                let sent = self.send_now(&ClientMessage::Request(ControlRequest {
                    id,
                    request: Request::FetchScrollback(req),
                }));
                if let Err(e) = sent {
                    if let Ok(mut w) = router.waiters.lock() {
                        w.remove(&id);
                    }
                    return Err(e);
                }
                match rx.await {
                    Ok(Ok(Response::Scrollback(page))) => Ok(Ok(page)),
                    Ok(Ok(other)) => Err(TransportError::ProtocolViolation(format!(
                        "unexpected response to FetchScrollback: {other:?}"
                    ))),
                    Ok(Err(rpc)) => Ok(Err(rpc)),
                    Err(_) => Err(closed_err(&self.inner.closer)),
                }
            }
        }
    }

    /// Delegates to `QuicClient::rebind`; Ok(()) no-op for stream links.
    pub fn rebind(&self) -> std::io::Result<()> {
        match &self.inner.quic {
            Some((_, endpoint)) => crate::quic::rebind_endpoint(endpoint),
            None => Ok(()),
        }
    }

    /// Smoothed RTT (QUIC), None for stream links.
    pub fn rtt(&self) -> Option<Duration> {
        self.inner.quic.as_ref().map(|(c, _)| c.rtt())
    }

    /// Send `ClientMessage::Goodbye`, then close with code 0.
    pub async fn close(&self) {
        if self.inner.closer.is_closed() {
            self.abort("closed");
            return;
        }
        let _ = self.send_now(&ClientMessage::Goodbye);
        finish_writer(&self.inner.writer).await;
        if let Some((conn, _)) = &self.inner.quic {
            conn.close(close_code::NORMAL.into(), b"goodbye");
        }
        self.inner.closer.close("closed by client");
        self.inner.tasks.abort_all();
        self.inner.scroll.clear();
    }

    /// Drop the link without a Goodbye (the daemon keeps resume state).
    pub(crate) fn abort(&self, reason: &str) {
        if let Some((conn, _)) = &self.inner.quic {
            conn.close(close_code::NORMAL.into(), reason.as_bytes());
        }
        self.inner.closer.close(reason.to_string());
        self.inner.tasks.abort_all();
        self.inner.scroll.clear();
    }

    /// Resolves when the connection is gone.
    pub async fn closed(&self) -> TransportError {
        match &self.inner.quic {
            Some((conn, _)) => crate::error::from_conn_error(&conn.closed().await),
            None => TransportError::Closed(self.inner.closer.wait().await),
        }
    }
}

/// Everything the daemon sends on one connection.
pub struct ClientReceiver {
    pub(crate) rx: mpsc::UnboundedReceiver<RxItem<ServerMessage>>,
    pub(crate) done: bool,
    /// Keeps the link alive while only the receiver is held.
    pub(crate) _shared: Arc<ClientShared>,
}

impl fmt::Debug for ClientReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientReceiver")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl ClientReceiver {
    /// Next `ServerMessage` from the control stream or any screen stream
    /// (screen streams are read as they are opened by the daemon). Per pane,
    /// screen updates are delivered in order. Ok(None) on clean close.
    /// Cancel-safe. Scrollback responses on stream links are routed to
    /// `fetch_scrollback` callers, not returned here.
    pub async fn recv(&mut self) -> Result<Option<ServerMessage>, TransportError> {
        if self.done {
            return Ok(None);
        }
        match self.rx.recv().await {
            Some(RxItem::Item(m)) => Ok(Some(m)),
            Some(RxItem::Failed(e)) => {
                self.done = true;
                Err(e)
            }
            Some(RxItem::End) | None => {
                self.done = true;
                Ok(None)
            }
        }
    }
}

/// Map a framing error on a QUIC stream of `conn` into a terminal item:
/// local/normal closes are a clean end.
pub(crate) fn quic_end_item<T>(conn: &quinn::Connection, e: civet_proto::ProtoError) -> RxItem<T> {
    match conn.close_reason() {
        Some(quinn::ConnectionError::LocallyClosed) => RxItem::End,
        Some(quinn::ConnectionError::ApplicationClosed(c))
            if c.error_code.into_inner() == u64::from(close_code::NORMAL) =>
        {
            RxItem::End
        }
        _ => RxItem::Failed(frame_error(conn, e)),
    }
}
