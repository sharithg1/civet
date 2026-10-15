//! QUIC (quinn) server and client.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use civet_proto::frame::read_raw_frame;
use civet_proto::{
    ALPN, CertFingerprint, ClientHello, ClientMessage, HelloReject, HelloReply, RpcError,
    ScrollbackPage, ScrollbackRequest, ServerMessage, SessionKey, StreamOpen, decode,
};
use quinn::crypto::rustls::{
    QuicClientConfig as QuinnClientCrypto, QuicServerConfig as QuinnServerCrypto,
};
use quinn::{IdleTimeout, TransportConfig, VarInt};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::cert::{PinnedVerifier, SERVER_NAME, ServerIdentity, provider};
use crate::conn::{
    ClientConnection, ClientReceiver, ClientShared, Closer, Connected, PendingServerLink,
    ResponderInner, RxItem, ScrollRouter, ScrollbackResponder, ServerConnection, ServerIncoming,
    Tasks, TransportKind, WriteCmd, classify_client_message, frame, quic_end_item, spawn_writer,
};
use crate::error::{TransportError, close_code, frame_error, from_conn_error, stream_error};

/// QUIC idle timeout: a laptop that wakes within 6 h continues the same
/// connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// Client keep-alive interval (keeps NAT bindings while awake).
pub const KEEP_ALIVE: Duration = Duration::from_secs(2);
/// Initial RTT estimate.
pub const INITIAL_RTT: Duration = Duration::from_millis(100);
/// Default client connect (handshake + hello) timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the daemon waits for the client's hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// mosh-style default daemon UDP port range.
pub const DEFAULT_PORT_RANGE: std::ops::RangeInclusive<u16> = 60001..=60999;

const MAX_BIDI_STREAMS: u32 = 64;
const MAX_UNI_STREAMS: u32 = 1024;

fn transport_config(client: bool) -> TransportConfig {
    let mut t = TransportConfig::default();
    t.max_idle_timeout(Some(
        IdleTimeout::try_from(IDLE_TIMEOUT).expect("6 h fits in a QUIC idle timeout"),
    ));
    t.initial_rtt(INITIAL_RTT);
    t.max_concurrent_bidi_streams(VarInt::from_u32(MAX_BIDI_STREAMS));
    t.max_concurrent_uni_streams(VarInt::from_u32(MAX_UNI_STREAMS));
    t.keep_alive_interval(client.then_some(KEEP_ALIVE));
    t
}

pub(crate) fn server_crypto(
    identity: &ServerIdentity,
) -> Result<quinn::ServerConfig, TransportError> {
    let tls = |e: rustls::Error| TransportError::Tls(e.to_string());
    let mut crypto = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls)?
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert()], identity.key())
        .map_err(tls)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let crypto =
        QuinnServerCrypto::try_from(crypto).map_err(|e| TransportError::Tls(e.to_string()))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(Arc::new(transport_config(false)));
    cfg.migration(true);
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Daemon QUIC listener settings.
#[derive(Debug)]
pub struct QuicServerConfig {
    /// e.g. 0.0.0.0:0. Use `bind_port_range` to choose the port.
    pub bind: SocketAddr,
    /// TLS identity (cert pinned by clients).
    pub identity: ServerIdentity,
    /// Session key clients must present.
    pub session_key: SessionKey,
}

/// UDP/QUIC listener. Clone-free; share via Arc.
pub struct QuicServer {
    endpoint: quinn::Endpoint,
    fingerprint: CertFingerprint,
    session_key: Arc<SessionKey>,
    closed: AtomicBool,
}

impl fmt::Debug for QuicServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuicServer")
            .field("local_addr", &self.endpoint.local_addr().ok())
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl QuicServer {
    /// Bind on `config.bind`.
    pub fn bind(config: QuicServerConfig) -> Result<Self, TransportError> {
        let server_config = server_crypto(&config.identity)?;
        let endpoint = quinn::Endpoint::server(server_config, config.bind)?;
        Ok(Self {
            endpoint,
            fingerprint: config.identity.fingerprint(),
            session_key: Arc::new(config.session_key),
            closed: AtomicBool::new(false),
        })
    }

    /// Try ports in `range` in order (mosh-style 60001..=60999) on `ip`; first
    /// that binds wins.
    pub fn bind_port_range(
        ip: IpAddr,
        range: std::ops::RangeInclusive<u16>,
        identity: ServerIdentity,
        session_key: SessionKey,
    ) -> Result<Self, TransportError> {
        let server_config = server_crypto(&identity)?;
        let mut last_err = None;
        for port in range.clone() {
            match quinn::Endpoint::server(server_config.clone(), SocketAddr::new(ip, port)) {
                Ok(endpoint) => {
                    return Ok(Self {
                        endpoint,
                        fingerprint: identity.fingerprint(),
                        session_key: Arc::new(session_key),
                        closed: AtomicBool::new(false),
                    });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(TransportError::Io(last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("empty port range {range:?}"),
            )
        })))
    }

    /// Bound address.
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
    }

    /// Fingerprint of the served certificate.
    pub fn fingerprint(&self) -> CertFingerprint {
        self.fingerprint
    }

    /// Next incoming connection; None once `close()` was called. Returns
    /// quickly: do the handshake in a spawned task.
    pub async fn accept(&self) -> Option<IncomingConnection> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let incoming = self.endpoint.accept().await?;
        Some(IncomingConnection {
            incoming,
            session_key: self.session_key.clone(),
        })
    }

    /// Close all connections with code 4 and stop accepting.
    pub fn close(&self, reason: &str) {
        self.closed.store(true, Ordering::Release);
        self.endpoint
            .close(close_code::SHUTTING_DOWN.into(), reason.as_bytes());
    }
}

/// A connection attempt that has not done its handshake yet.
pub struct IncomingConnection {
    incoming: quinn::Incoming,
    session_key: Arc<SessionKey>,
}

impl fmt::Debug for IncomingConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IncomingConnection")
            .field("remote_addr", &self.incoming.remote_address())
            .finish()
    }
}

impl IncomingConnection {
    /// Peer address.
    pub fn remote_addr(&self) -> SocketAddr {
        self.incoming.remote_address()
    }

    /// TLS handshake, accept the control stream, read `StreamOpen::Control` +
    /// `ClientHello` (10 s timeout), and validate magic / version / session
    /// key with `ClientHello::validate`. On failure it sends
    /// `HelloReply::Rejected` itself, closes, and returns `Err(Rejected)`.
    /// On success the hello is NOT yet answered: the daemon decides about
    /// resume and calls `ServerConnection::accept`.
    pub async fn handshake(self) -> Result<ServerConnection, TransportError> {
        let connecting = self.incoming.accept().map_err(|e| from_conn_error(&e))?;
        let key = self.session_key;
        tokio::time::timeout(HELLO_TIMEOUT, async move {
            let conn = connecting.await.map_err(|e| from_conn_error(&e))?;
            let (send, mut recv) = conn.accept_bi().await.map_err(|e| from_conn_error(&e))?;
            let open = read_raw_frame(&mut recv)
                .await
                .map_err(|e| frame_error(&conn, e))?
                .ok_or_else(|| TransportError::ProtocolViolation("control stream ended".into()))?;
            if decode::<StreamOpen>(&open).ok() != Some(StreamOpen::Control) {
                conn.close(close_code::PROTOCOL_VIOLATION.into(), b"expected control stream");
                return Err(TransportError::ProtocolViolation(
                    "first stream is not a control stream".into(),
                ));
            }
            let payload = read_raw_frame(&mut recv)
                .await
                .map_err(|e| frame_error(&conn, e))?
                .ok_or_else(|| TransportError::ProtocolViolation("no hello".into()))?;
            match crate::parse_hello(&payload, &key) {
                Ok(hello) => Ok(ServerConnection {
                    hello,
                    link: PendingServerLink::Quic { conn, send, recv },
                }),
                Err(reject) => {
                    tracing::info!(reason = ?reject.reason, remote = %conn.remote_address(), "rejecting hello");
                    reject_quic(&conn, send, &reject).await;
                    Err(TransportError::Rejected(reject))
                }
            }
        })
        .await
        .map_err(|_| TransportError::Timeout("waiting for client hello"))?
    }
}

/// Send `HelloReply::Rejected`, wait briefly for delivery, close with code 1
/// (the close reason also carries the encoded rejection).
pub(crate) async fn reject_quic(
    conn: &quinn::Connection,
    mut send: quinn::SendStream,
    reject: &HelloReject,
) {
    if let Ok(bytes) = frame(&HelloReply::Rejected(reject.clone())) {
        let _ = send.write_all(&bytes).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(1), send.stopped()).await;
    }
    let reason = civet_proto::encode(reject).unwrap_or_default();
    conn.close(close_code::REJECTED.into(), &reason);
}

/// Reader tasks for an accepted daemon-side QUIC connection: the control
/// stream plus one task per client-opened scrollback stream.
pub(crate) fn spawn_server_readers(
    conn: quinn::Connection,
    mut recv: quinn::RecvStream,
    tx: mpsc::Sender<RxItem<ServerIncoming>>,
    writer: mpsc::UnboundedSender<WriteCmd>,
) -> AbortHandle {
    tokio::spawn(async move {
        let control = {
            let conn = conn.clone();
            let tx = tx.clone();
            async move {
                loop {
                    match read_raw_frame(&mut recv).await {
                        Ok(Some(payload)) => match decode::<ClientMessage>(&payload) {
                            Ok(ClientMessage::Goodbye) => {
                                let _ = tx
                                    .send(RxItem::Item(ServerIncoming::Message(
                                        ClientMessage::Goodbye,
                                    )))
                                    .await;
                                let _ = tx.send(RxItem::End).await;
                                return;
                            }
                            Ok(msg) => {
                                let item = classify_client_message(msg, &writer);
                                if tx.send(RxItem::Item(item)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                conn.close(
                                    close_code::PROTOCOL_VIOLATION.into(),
                                    b"undecodable control frame",
                                );
                                let _ = tx
                                    .send(RxItem::Failed(TransportError::ProtocolViolation(
                                        format!("bad control frame: {e}"),
                                    )))
                                    .await;
                                return;
                            }
                        },
                        Ok(None) => {
                            let _ = tx.send(RxItem::End).await;
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(quic_end_item(&conn, e)).await;
                            return;
                        }
                    }
                }
            }
        };
        let streams = async move {
            while let Ok((send, recv)) = conn.accept_bi().await {
                let conn = conn.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    match read_scrollback_request(recv).await {
                        Ok(request) => {
                            let responder = ScrollbackResponder {
                                inner: ResponderInner::Quic { send, conn },
                            };
                            let _ = tx
                                .send(RxItem::Item(ServerIncoming::Scrollback {
                                    request,
                                    responder,
                                }))
                                .await;
                        }
                        Err(e) => tracing::debug!(error = %e, "bad scrollback stream"),
                    }
                });
            }
        };
        // The control reader decides when the receiver ends; the stream
        // acceptor stops with the connection.
        tokio::select! {
            _ = control => {},
            _ = streams => {},
        }
    })
    .abort_handle()
}

async fn read_scrollback_request(
    mut recv: quinn::RecvStream,
) -> Result<ScrollbackRequest, TransportError> {
    tokio::time::timeout(HELLO_TIMEOUT, async move {
        let open = read_raw_frame(&mut recv)
            .await?
            .ok_or_else(|| TransportError::ProtocolViolation("empty stream".into()))?;
        if decode::<StreamOpen>(&open)? != StreamOpen::Scrollback {
            return Err(TransportError::ProtocolViolation(
                "client-opened stream is not a scrollback stream".into(),
            ));
        }
        let req = read_raw_frame(&mut recv)
            .await?
            .ok_or_else(|| TransportError::ProtocolViolation("no scrollback request".into()))?;
        Ok(decode::<ScrollbackRequest>(&req)?)
    })
    .await
    .map_err(|_| TransportError::Timeout("scrollback request"))?
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Where and how to dial the daemon.
#[derive(Debug, Clone)]
pub struct QuicClientConfig {
    /// Resolved daemon address (host from `ssh -G`, port from bootstrap).
    pub addr: SocketAddr,
    /// Pinned certificate fingerprint (from bootstrap).
    pub fingerprint: CertFingerprint,
    /// Handshake + hello timeout; default 5 s.
    pub connect_timeout: Duration,
}

impl QuicClientConfig {
    /// Config with the default connect timeout.
    pub fn new(addr: SocketAddr, fingerprint: CertFingerprint) -> Self {
        Self {
            addr,
            fingerprint,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }
}

/// Owns the client UDP endpoint. One per host is fine; share via Arc.
pub struct QuicClient {
    endpoint: quinn::Endpoint,
}

impl fmt::Debug for QuicClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuicClient")
            .field("local_addr", &self.endpoint.local_addr().ok())
            .finish()
    }
}

fn unspecified(ipv6: bool) -> SocketAddr {
    if ipv6 {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    }
}

/// Swap the endpoint's UDP socket for a freshly bound one of the same family.
pub(crate) fn rebind_endpoint(endpoint: &quinn::Endpoint) -> std::io::Result<()> {
    let ipv6 = endpoint.local_addr()?.is_ipv6();
    let socket = std::net::UdpSocket::bind(unspecified(ipv6))?;
    endpoint.rebind(socket)
}

impl QuicClient {
    /// Bind an endpoint on an unspecified address of the given family
    /// (0.0.0.0:0 or [::]:0).
    pub fn new(ipv6: bool) -> Result<Self, TransportError> {
        Ok(Self {
            endpoint: quinn::Endpoint::client(unspecified(ipv6))?,
        })
    }

    /// Local UDP address of the endpoint (changes on `rebind`).
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Replace the endpoint's UDP socket with a freshly bound one
    /// (`quinn::Endpoint::rebind(std::net::UdpSocket::bind(..)?)`). Every
    /// connection on the endpoint migrates to the new socket; the server
    /// follows on the next packet. Call after wake / network change.
    pub fn rebind(&self) -> std::io::Result<()> {
        rebind_endpoint(&self.endpoint)
    }

    /// Dial, pin the cert, open the control stream, send
    /// `StreamOpen::Control` + `hello`, flush, then send each of `pipelined`
    /// (e.g. SubscribePane requests) **before** awaiting the HelloReply.
    pub async fn connect(
        &self,
        config: &QuicClientConfig,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> Result<Connected, TransportError> {
        let (verifier, mismatch) = PinnedVerifier::new(config.fingerprint, provider());
        let tls = |e: rustls::Error| TransportError::Tls(e.to_string());
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(tls)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![ALPN.to_vec()];
        let crypto =
            QuinnClientCrypto::try_from(crypto).map_err(|e| TransportError::Tls(e.to_string()))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
        client_config.transport_config(Arc::new(transport_config(true)));

        let expected = config.fingerprint;
        let mismatch_err = || {
            mismatch
                .lock()
                .ok()
                .and_then(|s| *s)
                .map(|actual| TransportError::FingerprintMismatch { expected, actual })
        };

        let attempt = async {
            let connecting = self
                .endpoint
                .connect_with(client_config, config.addr, SERVER_NAME)
                .map_err(|e| TransportError::Quic(e.to_string()))?;
            let conn = match connecting.await {
                Ok(c) => c,
                Err(e) => return Err(mismatch_err().unwrap_or_else(|| from_conn_error(&e))),
            };
            let (mut send, mut recv) = conn.open_bi().await.map_err(|e| from_conn_error(&e))?;
            let mut buf = Vec::new();
            buf.extend_from_slice(&frame(&StreamOpen::Control)?);
            buf.extend_from_slice(&frame(&hello)?);
            for m in &pipelined {
                buf.extend_from_slice(&frame(m)?);
            }
            send.write_all(&buf)
                .await
                .map_err(|e| stream_error(&conn, e))?;
            let reply = read_raw_frame(&mut recv)
                .await
                .map_err(|e| frame_error(&conn, e))?;
            let reply = match reply {
                Some(r) => decode::<HelloReply>(&r)?,
                None => {
                    return Err(conn
                        .close_reason()
                        .map(|r| from_conn_error(&r))
                        .unwrap_or_else(|| {
                            TransportError::ProtocolViolation("control stream ended".into())
                        }));
                }
            };
            match reply {
                HelloReply::Rejected(r) => {
                    conn.close(close_code::NORMAL.into(), b"rejected");
                    Err(TransportError::Rejected(r))
                }
                HelloReply::Accepted(server_hello) => Ok(start_client(
                    conn,
                    self.endpoint.clone(),
                    send,
                    recv,
                    server_hello,
                )),
            }
        };
        match tokio::time::timeout(config.connect_timeout, attempt).await {
            Ok(r) => r,
            Err(_) => Err(mismatch_err().unwrap_or(TransportError::Timeout("quic connect"))),
        }
    }
}

fn start_client(
    conn: quinn::Connection,
    endpoint: quinn::Endpoint,
    send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    server_hello: civet_proto::ServerHello,
) -> Connected {
    let closer = Closer::new();
    let tasks = Arc::new(Tasks::default());
    let (wtx, wrx) = mpsc::unbounded_channel();
    let (itx, irx) = mpsc::unbounded_channel();
    tasks.push(spawn_writer(send, wrx, closer.clone()));

    // Control stream reader.
    {
        let conn = conn.clone();
        let itx = itx.clone();
        let closer = closer.clone();
        tasks.push(
            tokio::spawn(async move {
                loop {
                    match read_raw_frame(&mut recv).await {
                        Ok(Some(payload)) => match decode::<ServerMessage>(&payload) {
                            Ok(msg) => {
                                if itx.send(RxItem::Item(msg)).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                conn.close(
                                    close_code::PROTOCOL_VIOLATION.into(),
                                    b"undecodable control frame",
                                );
                                closer.close("protocol violation");
                                let _ =
                                    itx.send(RxItem::Failed(TransportError::ProtocolViolation(
                                        format!("bad control frame: {e}"),
                                    )));
                                return;
                            }
                        },
                        Ok(None) => {
                            closer.close("control stream finished by daemon");
                            let _ = itx.send(RxItem::End);
                            return;
                        }
                        Err(e) => {
                            closer.close("connection lost");
                            let _ = itx.send(quic_end_item(&conn, e));
                            return;
                        }
                    }
                }
            })
            .abort_handle(),
        );
    }

    // Screen streams opened by the daemon.
    {
        let conn = conn.clone();
        let readers = tasks.clone();
        tasks.push(
            tokio::spawn(async move {
                while let Ok(mut uni) = conn.accept_uni().await {
                    let itx = itx.clone();
                    let h = tokio::spawn(async move {
                        let pane = match read_raw_frame(&mut uni).await {
                            Ok(Some(p)) => match decode::<StreamOpen>(&p) {
                                Ok(StreamOpen::Screen { pane }) => pane,
                                other => {
                                    tracing::warn!(?other, "unexpected uni stream header");
                                    return;
                                }
                            },
                            _ => return,
                        };
                        loop {
                            match read_raw_frame(&mut uni).await {
                                Ok(Some(p)) => match decode::<ServerMessage>(&p) {
                                    Ok(ServerMessage::Screen(u)) if u.pane == pane => {
                                        if itx.send(RxItem::Item(ServerMessage::Screen(u))).is_err()
                                        {
                                            return;
                                        }
                                    }
                                    other => {
                                        tracing::warn!(
                                            %pane,
                                            ok = other.is_ok(),
                                            "unexpected frame on screen stream"
                                        );
                                        return;
                                    }
                                },
                                Ok(None) => return,
                                Err(e) => {
                                    tracing::debug!(%pane, error = %e, "screen stream ended");
                                    return;
                                }
                            }
                        }
                    });
                    readers.push(h.abort_handle());
                }
            })
            .abort_handle(),
        );
    }

    let shared = Arc::new(ClientShared {
        kind: TransportKind::Quic,
        writer: wtx,
        closer,
        quic: Some((conn, endpoint)),
        scroll: Arc::new(ScrollRouter::default()),
        tasks,
    });
    Connected {
        conn: ClientConnection {
            inner: shared.clone(),
        },
        incoming: ClientReceiver {
            rx: irx,
            done: false,
            _shared: shared,
        },
        server_hello,
    }
}

/// One scrollback exchange on its own bidi stream.
pub(crate) async fn fetch_scrollback(
    conn: &quinn::Connection,
    req: ScrollbackRequest,
) -> Result<Result<ScrollbackPage, RpcError>, TransportError> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| from_conn_error(&e))?;
    let mut buf = frame(&StreamOpen::Scrollback)?.to_vec();
    buf.extend_from_slice(&frame(&req)?);
    send.write_all(&buf)
        .await
        .map_err(|e| stream_error(conn, e))?;
    send.finish().map_err(|e| stream_error(conn, e))?;
    let payload = read_raw_frame(&mut recv)
        .await
        .map_err(|e| frame_error(conn, e))?
        .ok_or_else(|| TransportError::Closed("scrollback stream ended without a reply".into()))?;
    Ok(decode::<Result<ScrollbackPage, RpcError>>(&payload)?)
}
