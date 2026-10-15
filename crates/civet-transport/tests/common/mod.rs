//! A tiny in-process "daemon" for transport tests: resume tokens, input
//! dedup, one screen stream per subscribed pane, scrollback, ping, events.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use civet_proto::{
    ClientId, ClientMessage, ControlRequest, ControlResponse, ErrorCode, Event, InputMessage,
    PaneId, Request, Response, ResumeToken, RpcError, ScreenUpdate, ScrollbackLine, ScrollbackPage,
    ScrollbackQuery, ServerHello, ServerMessage, SessionKey, SubscribeInfo, TermSize, UpdateKind,
};
use civet_transport::{
    ClientSessionConfig, QuicServer, QuicServerConfig, ScreenStream, ServerConnection,
    ServerIdentity, ServerIncoming, ServerSender, accept_stream,
};

#[derive(Default)]
pub struct ClientState {
    token: Option<ResumeToken>,
    prev: Option<ResumeToken>,
    /// Last applied input seq per pane.
    pub applied: HashMap<PaneId, u64>,
    pub subs: HashSet<PaneId>,
    conn: Option<ServerSender>,
    /// Every input frame received (pane, seq), including duplicates.
    pub received: Vec<(PaneId, u64)>,
    /// Inputs written "to the PTY" in order.
    pub applied_log: Vec<(PaneId, u64, Vec<u8>)>,
    pub goodbye: bool,
}

pub struct TestDaemon {
    pub key: Mutex<SessionKey>,
    pub instance_id: AtomicU64,
    pub clients: Mutex<HashMap<ClientId, ClientState>>,
    /// Per-pane screen state number.
    pane_state: Mutex<HashMap<PaneId, u64>>,
    serial: AtomicU64,
    /// Connections with serial below this ignore everything (dead link).
    pub muted_below: AtomicU64,
    pub reject_resume: AtomicBool,
    /// Drop the connection (without applying) when this seq arrives.
    pub drop_at_seq: Mutex<Option<u64>>,
    /// Next screen update is sent with a wrong base state.
    pub bogus_diff_next: AtomicBool,
    pub subscribe_requests: Mutex<Vec<(PaneId, Option<u64>)>>,
    pub connections: AtomicU64,
    /// Panes that no longer exist (SubscribePane -> NotFound).
    pub closed_panes: Mutex<HashSet<PaneId>>,
    /// Ignore (neither apply nor ack) this many next input frames, like a
    /// daemon refusing input under PTY backpressure.
    pub refuse_inputs: Mutex<u64>,
}

pub fn hello_template() -> (SessionKey, ClientId) {
    (
        SessionKey::generate().unwrap(),
        ClientId::generate().unwrap(),
    )
}

pub fn session_config(key: SessionKey, client_id: ClientId) -> ClientSessionConfig {
    ClientSessionConfig::new(key, client_id, "test-client", "0.0.0")
}

impl TestDaemon {
    pub fn new(key: SessionKey) -> Arc<Self> {
        Arc::new(Self {
            key: Mutex::new(key),
            instance_id: AtomicU64::new(1),
            clients: Mutex::new(HashMap::new()),
            pane_state: Mutex::new(HashMap::new()),
            serial: AtomicU64::new(0),
            muted_below: AtomicU64::new(0),
            reject_resume: AtomicBool::new(false),
            drop_at_seq: Mutex::new(None),
            bogus_diff_next: AtomicBool::new(false),
            subscribe_requests: Mutex::new(Vec::new()),
            connections: AtomicU64::new(0),
            closed_panes: Mutex::new(HashSet::new()),
            refuse_inputs: Mutex::new(0),
        })
    }

    /// Start a QUIC listener on 127.0.0.1 and serve connections.
    pub fn spawn_quic(self: &Arc<Self>) -> (Arc<QuicServer>, civet_proto::CertFingerprint) {
        let identity = ServerIdentity::generate().unwrap();
        let fp = identity.fingerprint();
        let server = Arc::new(
            QuicServer::bind(QuicServerConfig {
                bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                identity,
                session_key: self.key.lock().unwrap().clone(),
            })
            .unwrap(),
        );
        let s = server.clone();
        let d = self.clone();
        tokio::spawn(async move {
            while let Some(inc) = s.accept().await {
                let d = d.clone();
                tokio::spawn(async move {
                    if let Ok(conn) = inc.handshake().await {
                        d.serve(conn).await;
                    }
                });
            }
        });
        (server, fp)
    }

    /// Serve one byte-stream link (the other end of a duplex).
    pub async fn serve_stream(self: Arc<Self>, io: tokio::io::DuplexStream) {
        let key = self.key.lock().unwrap().clone();
        if let Ok(conn) = accept_stream(io, &key).await {
            self.serve(conn).await;
        }
    }

    pub fn client(
        &self,
        id: &ClientId,
    ) -> std::sync::MutexGuard<'_, HashMap<ClientId, ClientState>> {
        let g = self.clients.lock().unwrap();
        assert!(g.contains_key(id));
        g
    }

    pub fn remote_addr(&self, id: &ClientId) -> Option<SocketAddr> {
        self.clients
            .lock()
            .unwrap()
            .get(id)?
            .conn
            .as_ref()?
            .remote_addr()
    }

    /// Kill the current connection of every client (no Goodbye).
    pub fn drop_all(&self) {
        for st in self.clients.lock().unwrap().values_mut() {
            if let Some(c) = st.conn.take() {
                c.close(2, "test: connection dropped");
            }
        }
    }

    /// Make all current connections unresponsive.
    pub fn mute_current(&self) {
        let s = self.serial.load(Ordering::SeqCst);
        self.muted_below.store(s, Ordering::SeqCst);
    }

    pub async fn serve(self: Arc<Self>, conn: ServerConnection) {
        let hello = conn.hello().clone();
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        self.connections.fetch_add(1, Ordering::SeqCst);
        let (resumed, token, infos) = {
            let mut clients = self.clients.lock().unwrap();
            let entry = clients.entry(hello.client_id).or_default();
            let resumed = !self.reject_resume.load(Ordering::SeqCst)
                && hello.resume.as_ref().is_some_and(|t| {
                    entry.token.as_ref().is_some_and(|c| c.ct_eq(t))
                        || entry.prev.as_ref().is_some_and(|c| c.ct_eq(t))
                });
            if !resumed {
                entry.applied.clear();
                entry.subs.clear();
            }
            if let Some(old) = entry.conn.take() {
                old.close(3, "replaced");
            }
            let token = ResumeToken::generate().unwrap();
            entry.prev = entry.token.replace(token.clone());
            let states = self.pane_state.lock().unwrap();
            let infos: Vec<SubscribeInfo> = if resumed {
                let closed = self.closed_panes.lock().unwrap();
                entry
                    .subs
                    .iter()
                    .filter(|p| !closed.contains(p))
                    .map(|p| SubscribeInfo {
                        pane: *p,
                        state: states.get(p).copied().unwrap_or(1),
                        size: TermSize::new(80, 24),
                        history_lines: 0,
                        last_input_seq: entry.applied.get(p).copied().unwrap_or(0),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (resumed, token, infos)
        };
        let accepted = conn
            .accept(ServerHello {
                proto_version: civet_proto::PROTOCOL_VERSION,
                daemon_version: "test".into(),
                instance_id: self.instance_id.load(Ordering::SeqCst),
                resumed,
                server_time_ms: civet_proto::now_ms(),
                resume_token: token,
                resumed_subscriptions: infos,
            })
            .await;
        let Ok((tx, mut rx)) = accepted else { return };
        self.clients
            .lock()
            .unwrap()
            .get_mut(&hello.client_id)
            .unwrap()
            .conn = Some(tx.clone());
        let cid = hello.client_id;
        let mut screens: HashMap<PaneId, ScreenStream> = HashMap::new();

        while let Ok(Some(item)) = rx.recv().await {
            if serial <= self.muted_below.load(Ordering::SeqCst) {
                continue;
            }
            match item {
                ServerIncoming::Message(ClientMessage::Request(ControlRequest { id, request })) => {
                    let result = match request {
                        Request::Ping { nonce } => Ok(Response::Pong {
                            nonce,
                            server_time_ms: civet_proto::now_ms(),
                        }),
                        Request::ListRepos => {
                            let _ = tx.send(ServerMessage::Event(Event::ReposChanged)).await;
                            Ok(Response::Repos(vec![]))
                        }
                        Request::SubscribePane { pane, .. }
                            if self.closed_panes.lock().unwrap().contains(&pane) =>
                        {
                            if let Some(st) = self.clients.lock().unwrap().get_mut(&cid) {
                                st.subs.remove(&pane);
                            }
                            Err(RpcError::not_found(pane))
                        }
                        Request::SubscribePane { pane, known_state } => {
                            self.subscribe_requests
                                .lock()
                                .unwrap()
                                .push((pane, known_state));
                            if let Some(old) = screens.remove(&pane) {
                                old.finish().await;
                            }
                            let mut stream = match tx.open_screen_stream(pane).await {
                                Ok(s) => s,
                                Err(_) => break,
                            };
                            let state = *self.pane_state.lock().unwrap().entry(pane).or_insert(1);
                            let last = {
                                let mut c = self.clients.lock().unwrap();
                                let st = c.get_mut(&cid).unwrap();
                                st.subs.insert(pane);
                                st.applied.get(&pane).copied().unwrap_or(0)
                            };
                            let full = ScreenUpdate {
                                pane,
                                state,
                                base_state: 0,
                                kind: UpdateKind::Full,
                                acked_input_seq: last,
                                size: TermSize::new(80, 24),
                                history_lines: 0,
                                history_start: 0,
                                payload: format!("\x1bc[full {state}]").into_bytes(),
                            };
                            if stream.send(&full).await.is_err() {
                                break;
                            }
                            screens.insert(pane, stream);
                            Ok(Response::Subscribed(SubscribeInfo {
                                pane,
                                state,
                                size: TermSize::new(80, 24),
                                history_lines: 0,
                                last_input_seq: last,
                            }))
                        }
                        Request::UnsubscribePane { pane } => {
                            if let Some(old) = screens.remove(&pane) {
                                old.finish().await;
                            }
                            if let Some(st) = self.clients.lock().unwrap().get_mut(&cid) {
                                st.subs.remove(&pane);
                            }
                            Ok(Response::Ok)
                        }
                        _ => Err(RpcError::new(ErrorCode::Unsupported, "test daemon")),
                    };
                    if tx
                        .send(ServerMessage::Response(ControlResponse { id, result }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                ServerIncoming::Message(ClientMessage::Input(InputMessage { pane, seq, data })) => {
                    let refuse = {
                        let mut n = self.refuse_inputs.lock().unwrap();
                        let r = *n > 0;
                        *n = n.saturating_sub(1);
                        r
                    };
                    if refuse {
                        self.clients
                            .lock()
                            .unwrap()
                            .get_mut(&cid)
                            .unwrap()
                            .received
                            .push((pane, seq));
                        continue;
                    }
                    let drop_now = {
                        let mut d = self.drop_at_seq.lock().unwrap();
                        if *d == Some(seq) {
                            *d = None;
                            true
                        } else {
                            false
                        }
                    };
                    let apply = {
                        let mut c = self.clients.lock().unwrap();
                        let st = c.get_mut(&cid).unwrap();
                        st.received.push((pane, seq));
                        if drop_now {
                            false
                        } else {
                            let last = st.applied.entry(pane).or_insert(0);
                            if seq > *last {
                                *last = seq;
                                st.applied_log.push((pane, seq, data.clone()));
                                true
                            } else {
                                false
                            }
                        }
                    };
                    if drop_now {
                        tx.close(2, "test: drop at seq");
                        if let Some(st) = self.clients.lock().unwrap().get_mut(&cid) {
                            st.conn = None;
                        }
                        break;
                    }
                    if !apply {
                        continue;
                    }
                    let (base, state) = {
                        let mut ps = self.pane_state.lock().unwrap();
                        let s = ps.entry(pane).or_insert(1);
                        let base = *s;
                        *s += 1;
                        (base, *s)
                    };
                    if let Some(stream) = screens.get_mut(&pane) {
                        let bogus = self.bogus_diff_next.swap(false, Ordering::SeqCst);
                        let u = ScreenUpdate {
                            pane,
                            state,
                            base_state: if bogus { base + 1000 } else { base },
                            kind: UpdateKind::Diff,
                            acked_input_seq: seq,
                            size: TermSize::new(80, 24),
                            history_lines: 0,
                            history_start: 0,
                            payload: data,
                        };
                        if stream.send(&u).await.is_err() {
                            break;
                        }
                    }
                    let _ = tx.send(ServerMessage::InputAck { pane, seq }).await;
                }
                ServerIncoming::Message(ClientMessage::Ack(_)) => {}
                ServerIncoming::Message(ClientMessage::Goodbye) => {
                    if let Some(st) = self.clients.lock().unwrap().get_mut(&cid) {
                        st.goodbye = true;
                        st.conn = None;
                    }
                    break;
                }
                ServerIncoming::Scrollback { request, responder } => {
                    let (start, end) = match request.query {
                        ScrollbackQuery::Before { end, limit } => {
                            let end = end.unwrap_or(1000);
                            (end.saturating_sub(u64::from(limit)), end)
                        }
                        ScrollbackQuery::Range { start, end } => (start, end),
                    };
                    let page = ScrollbackPage {
                        req_id: request.req_id,
                        pane: request.pane,
                        start,
                        lines: (start..end)
                            .map(|i| ScrollbackLine {
                                vt: format!("line {i}").into_bytes(),
                                wrapped: false,
                            })
                            .collect(),
                        oldest_available: 0,
                        end_available: 1000,
                    };
                    let _ = responder.respond(Ok(page)).await;
                }
            }
        }
    }
}

/// Wait up to `t` for `f` to become true.
pub async fn eventually(t: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + t;
    while tokio::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f()
}
