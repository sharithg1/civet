//! Regression tests for review findings: two clients sharing a `ClientId`
//! (REPLACED storm), a pane closed while the client was away, input refused
//! under daemon backpressure (retransmit), and mid-session failover from
//! QUIC to a fallback link and back.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use civet_proto::{ClientHello, ClientMessage, Event, PaneId};
use civet_transport::{
    ClientSession, ClientSessionConfig, Connected, Dialer, FailoverConfig, FailoverDialer,
    LinkState, QuicClient, QuicClientConfig, QuicDialer, REPLACED_REASON, SessionEvent,
    StreamDialer, TransportError, TransportKind,
};
use common::{TestDaemon, eventually, hello_template, session_config};
use futures::future::BoxFuture;
use tokio::sync::mpsc::UnboundedReceiver;

const T: Duration = Duration::from_secs(10);
type Events = UnboundedReceiver<SessionEvent>;

fn fast(mut cfg: ClientSessionConfig) -> ClientSessionConfig {
    cfg.heartbeat = Duration::from_millis(100);
    cfg.stall_after = Duration::from_millis(150);
    cfg.race_after = Duration::from_millis(300);
    cfg.backoff = vec![Duration::ZERO];
    cfg
}

async fn next_event(ev: &mut Events, mut pred: impl FnMut(&SessionEvent) -> bool) -> SessionEvent {
    tokio::time::timeout(T, async {
        loop {
            let e = ev.recv().await.expect("event channel closed");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("timed out waiting for session event")
}

fn quic(daemon: &Arc<TestDaemon>) -> Arc<QuicDialer> {
    let (server, fp) = daemon.spawn_quic();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_addr().port())
        .parse()
        .unwrap();
    std::mem::forget(server);
    Arc::new(QuicDialer {
        client: Arc::new(QuicClient::new(false).unwrap()),
        config: QuicClientConfig::new(addr, fp),
    })
}

fn stream(daemon: &Arc<TestDaemon>) -> Arc<dyn Dialer> {
    let d = daemon.clone();
    Arc::new(StreamDialer {
        factory: move || {
            let d = d.clone();
            async move {
                let (a, b) = tokio::io::duplex(64 * 1024);
                tokio::spawn(d.serve_stream(b));
                Ok::<_, std::io::Error>(a)
            }
        },
    })
}

fn applied(daemon: &TestDaemon, cid: &civet_proto::ClientId) -> Vec<u64> {
    daemon.clients.lock().unwrap()[cid]
        .applied_log
        .iter()
        .map(|(_, s, _)| *s)
        .collect()
}

/// Two sessions with the same client id: each connect kicks the other off
/// (close code 3). Before the fix they redialed instantly forever (~600
/// reconnects in 2.5 s). Now a session taken over twice within
/// `REPLACED_WINDOW` stops with `Failed { REPLACED_REASON }`, and the total
/// number of connections stays tiny.
#[tokio::test]
async fn shared_client_id_does_not_reconnect_storm() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = quic(&daemon);
    let (a, mut a_ev) =
        ClientSession::connect(fast(session_config(key.clone(), cid)), dialer.clone())
            .await
            .unwrap();
    let (b, mut b_ev) = ClientSession::connect(fast(session_config(key, cid)), dialer)
        .await
        .unwrap();
    // Let them fight for a while.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let conns = daemon.connections.load(Ordering::SeqCst);
    assert!(conns <= 6, "reconnect storm: {conns} connections in 3 s");
    let failed = |s: &ClientSession| matches!(&*s.link_state().borrow(), LinkState::Failed { reason, needs_bootstrap: false } if reason == REPLACED_REASON);
    assert!(
        failed(&a) ^ failed(&b),
        "exactly one gives up: a={:?} b={:?}",
        *a.link_state().borrow(),
        *b.link_state().borrow()
    );
    let (winner, ev) = if failed(&a) {
        (&b, &mut b_ev)
    } else {
        (&a, &mut a_ev)
    };
    // The survivor still works.
    winner.subscribe(PaneId(1)).await.unwrap();
    next_event(ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    a.close().await;
    b.close().await;
}

/// A subscribed pane is closed while the client is disconnected: the
/// re-subscribe after the reconnect says NotFound, and the session reports
/// `PaneClosed` and stops buffering input for it.
#[tokio::test]
async fn pane_closed_while_away_is_reported() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (s, mut ev) = ClientSession::connect(fast(session_config(key, cid)), stream(&daemon))
        .await
        .unwrap();
    s.subscribe(PaneId(1)).await.unwrap();
    s.subscribe(PaneId(2)).await.unwrap();
    next_event(
        &mut ev,
        |e| matches!(e, SessionEvent::Screen(u) if u.pane == PaneId(2)),
    )
    .await;
    daemon.closed_panes.lock().unwrap().insert(PaneId(1));
    daemon.drop_all();
    let e = next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Event(Event::PaneClosed { .. }))
    })
    .await;
    assert!(matches!(
        e,
        SessionEvent::Event(Event::PaneClosed { pane: PaneId(1) })
    ));
    // Input for the closed pane is not buffered or sent; the other pane
    // still works.
    assert_eq!(s.send_input(PaneId(1), b"x".to_vec()), 0);
    let seq = s.send_input(PaneId(2), b"y".to_vec());
    next_event(
        &mut ev,
        |e| matches!(e, SessionEvent::InputAcked { pane: PaneId(2), seq: q } if *q == seq),
    )
    .await;
    assert!(
        !daemon.clients.lock().unwrap()[&cid]
            .received
            .iter()
            .any(|(p, _)| *p == PaneId(1))
    );
    s.close().await;
}

/// Input the daemon refuses (backpressure: neither applied nor acked) is
/// retransmitted on the same connection and applied exactly once, in order.
#[tokio::test]
async fn refused_input_is_retransmitted() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (s, mut ev) = ClientSession::connect(fast(session_config(key, cid)), stream(&daemon))
        .await
        .unwrap();
    s.subscribe(PaneId(1)).await.unwrap();
    *daemon.refuse_inputs.lock().unwrap() = 3;
    for b in [b"a", b"b", b"c"] {
        s.send_input(PaneId(1), b.to_vec());
    }
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 3, .. })
    })
    .await;
    assert_eq!(applied(&daemon, &cid), vec![1, 2, 3]);
    assert_eq!(
        daemon.connections.load(Ordering::SeqCst),
        1,
        "no reconnect needed"
    );
    s.close().await;
}

/// QUIC dialer that can be "blocked" (UDP dropped): dials then fail.
struct Blockable {
    inner: Arc<QuicDialer>,
    blocked: Arc<AtomicBool>,
}

impl Dialer for Blockable {
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        Box::pin(async move {
            if self.blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Err(TransportError::Timeout("blocked"));
            }
            self.inner.dial(hello, pipelined).await
        })
    }
    fn rebind(&self) -> std::io::Result<()> {
        self.inner.rebind()
    }
    fn kind(&self) -> TransportKind {
        TransportKind::Quic
    }
}

fn transport(s: &ClientSession) -> Option<TransportKind> {
    match &*s.link_state().borrow() {
        LinkState::Connected { transport, .. } => Some(*transport),
        _ => None,
    }
}

/// UDP gets blocked mid-session: the session fails over to the fallback
/// link (resuming, no input lost or duplicated); when UDP works again it
/// moves back to QUIC on its own.
#[tokio::test]
async fn failover_to_fallback_and_back() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let primary: Arc<dyn Dialer> = Arc::new(Blockable {
        inner: quic(&daemon),
        blocked: blocked.clone(),
    });
    let failover = Arc::new(FailoverDialer::new(
        primary,
        stream(&daemon),
        FailoverConfig {
            primary_timeout: Duration::from_millis(500),
            fail_over_after_attempts: 2,
            fail_over_after: Duration::from_millis(300),
            probe_interval: Duration::from_millis(300),
        },
    ));
    let (s, mut ev) = ClientSession::connect(fast(session_config(key, cid)), failover.clone())
        .await
        .unwrap();
    assert_eq!(transport(&s), Some(TransportKind::Quic));
    s.subscribe(PaneId(1)).await.unwrap();
    s.send_input(PaneId(1), b"1".to_vec());
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 1, .. })
    })
    .await;

    // UDP blackholed: the live QUIC connection goes silent, new dials fail.
    blocked.store(true, Ordering::SeqCst);
    daemon.mute_current();
    s.send_input(PaneId(1), b"2".to_vec());
    next_event(&mut ev, |e| {
        matches!(
            e,
            SessionEvent::Link(LinkState::Connected {
                transport: TransportKind::Stream,
                ..
            })
        )
    })
    .await;
    assert!(failover.on_fallback());
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 2, .. })
    })
    .await;
    s.send_input(PaneId(1), b"3".to_vec());
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 3, .. })
    })
    .await;

    // UDP back: the periodic probe moves the session back to QUIC.
    blocked.store(false, Ordering::SeqCst);
    next_event(&mut ev, |e| {
        matches!(
            e,
            SessionEvent::Link(LinkState::Connected {
                transport: TransportKind::Quic,
                ..
            })
        )
    })
    .await;
    assert!(!failover.on_fallback());
    s.send_input(PaneId(1), b"4".to_vec());
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 4, .. })
    })
    .await;
    assert!(eventually(T, || applied(&daemon, &cid) == vec![1, 2, 3, 4]).await);
    s.close().await;
}

/// UDP blocked from the start: the first connect itself falls back.
#[tokio::test]
async fn failover_on_first_connect() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let primary: Arc<dyn Dialer> = Arc::new(Blockable {
        inner: quic(&daemon),
        blocked: Arc::new(AtomicBool::new(true)),
    });
    let failover = Arc::new(FailoverDialer::new(
        primary,
        stream(&daemon),
        FailoverConfig {
            primary_timeout: Duration::from_millis(500),
            fail_over_after_attempts: 1,
            ..FailoverConfig::default()
        },
    ));
    let (s, _ev) = ClientSession::connect(fast(session_config(key, cid)), failover)
        .await
        .unwrap();
    assert_eq!(transport(&s), Some(TransportKind::Stream));
    s.close().await;
}

/// Before authentication the daemon reads tiny frames only: a StreamOpen or
/// hello that claims a large length is refused from its header, without
/// waiting for (or allocating) the claimed bytes.
#[tokio::test]
async fn pre_auth_frames_are_size_limited() {
    use tokio::io::AsyncWriteExt;
    let (key, _) = hello_template();
    for (claim, prefix) in [
        // StreamOpen claiming 1 MiB.
        (1u32 << 20, Vec::new()),
        // Valid StreamOpen::Control, then a hello claiming 16 MiB.
        (
            civet_proto::MAX_FRAME_LEN as u32,
            civet_proto::encode_frame(&civet_proto::StreamOpen::Control).unwrap(),
        ),
    ] {
        let (mut a, b) = tokio::io::duplex(1024);
        a.write_all(&prefix).await.unwrap();
        a.write_all(&claim.to_be_bytes()).await.unwrap();
        // Keep `a` open: the old code would wait for the claimed bytes.
        let r = tokio::time::timeout(
            Duration::from_secs(2),
            civet_transport::accept_stream(b, &key),
        )
        .await
        .expect("must fail fast, not wait for the claimed payload");
        assert!(
            matches!(
                r,
                Err(TransportError::Proto(
                    civet_proto::ProtoError::FrameTooLarge { .. }
                ))
            ),
            "{r:?}"
        );
        drop(a);
    }
}
