//! ClientSession tests: subscribe/screen/input/scrollback over both links,
//! migration via notify_network_change, resume after a full drop with exact-
//! once input resend, fresh-session semantics, stall → racing re-dial, diff
//! validation, daemon restart detection, and BadKey → Failed.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use civet_proto::{
    ClientHello, ClientId, ClientMessage, PaneId, Request, Response, ScrollbackQuery, SessionKey,
    UpdateKind,
};
use civet_transport::{
    ClientSession, ClientSessionConfig, Connected, Dialer, LinkState, QuicClient, QuicClientConfig,
    QuicDialer, SessionEvent, StreamDialer, TransportError, TransportKind,
};
use common::{TestDaemon, eventually, hello_template, session_config};
use futures::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

const T: Duration = Duration::from_secs(5);
type Events = UnboundedReceiver<SessionEvent>;

fn fast(mut cfg: ClientSessionConfig) -> ClientSessionConfig {
    cfg.heartbeat = Duration::from_millis(100);
    cfg.stall_after = Duration::from_millis(150);
    cfg.race_after = Duration::from_millis(300);
    cfg.backoff = vec![
        Duration::ZERO,
        Duration::from_millis(20),
        Duration::from_millis(50),
    ];
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

/// Connect and consume the initial `Connected` event.
async fn start(cfg: ClientSessionConfig, dialer: Arc<dyn Dialer>) -> (ClientSession, Events) {
    let (s, mut ev) = ClientSession::connect(cfg, dialer).await.unwrap();
    let e = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    assert!(matches!(
        e,
        SessionEvent::Connected {
            resumed: false,
            instance_changed: false,
            ..
        }
    ));
    (s, ev)
}

fn quic_dialer(daemon: &Arc<TestDaemon>) -> (Arc<QuicDialer>, Arc<QuicClient>) {
    let (server, fp) = daemon.spawn_quic();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_addr().port())
        .parse()
        .unwrap();
    std::mem::forget(server);
    let client = Arc::new(QuicClient::new(false).unwrap());
    (
        Arc::new(QuicDialer {
            client: client.clone(),
            config: QuicClientConfig::new(addr, fp),
        }),
        client,
    )
}

fn stream_dialer(daemon: &Arc<TestDaemon>) -> Arc<dyn Dialer> {
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

async fn basic(dialer: Arc<dyn Dialer>, daemon: Arc<TestDaemon>, key: SessionKey, cid: ClientId) {
    let kind = dialer.kind();
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    assert!(
        matches!(*s.link_state().borrow(), LinkState::Connected { transport, .. } if transport == kind)
    );

    let info = s.subscribe(PaneId(1)).await.unwrap();
    assert_eq!(info.pane, PaneId(1));
    let full = next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    assert!(matches!(full, SessionEvent::Screen(ref u) if u.kind == UpdateKind::Full));

    for i in 1..=10u64 {
        assert_eq!(s.send_input(PaneId(1), format!("k{i}").into_bytes()), i);
    }
    let mut last_diff_state = None;
    let mut acked = 0;
    while acked < 10 {
        match next_event(&mut ev, |_| true).await {
            SessionEvent::Screen(u) => {
                assert_eq!(u.kind, UpdateKind::Diff);
                if let Some(prev) = last_diff_state {
                    assert_eq!(u.base_state, prev);
                }
                last_diff_state = Some(u.state);
            }
            SessionEvent::InputAcked { pane, seq } => {
                assert_eq!(pane, PaneId(1));
                assert!(seq > acked);
                acked = seq;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    match s.request(Request::Ping { nonce: 5 }).await.unwrap() {
        Response::Pong { nonce, .. } => assert_eq!(nonce, 5),
        other => panic!("{other:?}"),
    }
    let err = s.request(Request::GetDaemonInfo).await.unwrap_err();
    assert!(matches!(err, civet_transport::RequestError::Rpc(_)));

    let page = s
        .fetch_scrollback(
            PaneId(1),
            ScrollbackQuery::Before {
                end: None,
                limit: 50,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.lines.len(), 50);
    assert_eq!(page.start, 950);

    // Heartbeats keep the link Connected while idle.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(matches!(
        *s.link_state().borrow(),
        LinkState::Connected { .. }
    ));

    s.unsubscribe(PaneId(1)).await.unwrap();
    s.close().await;
    assert_eq!(*s.link_state().borrow(), LinkState::Closed);
    assert!(eventually(T, || daemon.clients.lock().unwrap()[&cid].goodbye).await);
    assert!(matches!(
        s.request(Request::ListPanes).await,
        Err(civet_transport::RequestError::Closed)
    ));
    let applied: Vec<u64> = daemon.clients.lock().unwrap()[&cid]
        .applied_log
        .iter()
        .map(|(_, s, _)| *s)
        .collect();
    assert_eq!(applied, (1..=10).collect::<Vec<_>>());
}

#[tokio::test]
async fn session_basic_quic() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _) = quic_dialer(&daemon);
    basic(dialer, daemon, key, cid).await;
}

#[tokio::test]
async fn session_basic_stream() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    basic(dialer, daemon, key, cid).await;
}

#[tokio::test]
async fn session_bad_key_fails_first_connect() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key);
    let (dialer, _) = quic_dialer(&daemon);
    let err = ClientSession::connect(session_config(SessionKey([3; 32]), cid), dialer)
        .await
        .unwrap_err();
    assert!(err.is_fatal(), "{err:?}");
}

/// Network change: rebind + ping, same QUIC connection, no visible event.
#[tokio::test]
async fn session_network_change_migrates_quic() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, client) = quic_dialer(&daemon);
    let (s, mut ev) = start(session_config(key, cid), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;

    let mut timings = Vec::new();
    for i in 1..=5u64 {
        let before = client.local_addr().unwrap();
        s.notify_network_change();
        let t0 = Instant::now();
        let seq = s.send_input(PaneId(1), vec![b'a' + i as u8]);
        assert_eq!(seq, i);
        let e = next_event(&mut ev, |e| {
            matches!(
                e,
                SessionEvent::Screen(_)
                    | SessionEvent::Link(LinkState::Recovering { .. } | LinkState::Failed { .. })
                    | SessionEvent::Connected { .. }
            )
        })
        .await;
        timings.push(t0.elapsed());
        assert!(
            matches!(e, SessionEvent::Screen(ref u) if u.acked_input_seq == seq),
            "{e:?}"
        );
        let after = client.local_addr().unwrap();
        assert_ne!(before.port(), after.port());
        assert!(
            eventually(T, || daemon.remote_addr(&cid).map(|a| a.port())
                == Some(after.port()))
            .await
        );
    }
    eprintln!("session time-to-first-screen-update after notify_network_change: {timings:?}");
    assert!(
        timings.iter().all(|t| *t < Duration::from_millis(200)),
        "{timings:?}"
    );
    assert_eq!(
        daemon.connections.load(Ordering::SeqCst),
        1,
        "no reconnect happened"
    );
    assert!(matches!(
        *s.link_state().borrow(),
        LinkState::Connected { .. }
    ));
    s.close().await;
}

/// Full connection loss: resume with the token, resend unacked input exactly
/// once, keep numbering.
async fn resume_after_drop(
    dialer: Arc<dyn Dialer>,
    daemon: Arc<TestDaemon>,
    key: SessionKey,
    cid: ClientId,
) {
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;

    // The daemon drops the connection when seq 5 arrives, before applying it.
    *daemon.drop_at_seq.lock().unwrap() = Some(5);
    for i in 1..=10u64 {
        assert_eq!(s.send_input(PaneId(1), format!("{i};").into_bytes()), i);
    }
    let rec = next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Recovering { .. }))
    })
    .await;
    assert!(matches!(rec, SessionEvent::Link(_)));
    let t0 = Instant::now();
    let c = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    match c {
        SessionEvent::Connected {
            resumed,
            instance_changed,
            server_hello,
        } => {
            assert!(resumed);
            assert!(!instance_changed);
            assert_eq!(server_hello.resumed_subscriptions.len(), 1);
            assert_eq!(server_hello.resumed_subscriptions[0].last_input_seq, 4);
        }
        _ => unreachable!(),
    }
    eprintln!("resume after full drop took {:?}", t0.elapsed());

    // Typing continues with seq 11.
    for i in 11..=15u64 {
        assert_eq!(s.send_input(PaneId(1), format!("{i};").into_bytes()), i);
    }
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 15, .. })
    })
    .await;

    {
        let guard = daemon.clients.lock().unwrap();
        let st = &guard[&cid];
        let applied: Vec<u64> = st.applied_log.iter().map(|(_, s, _)| *s).collect();
        assert_eq!(
            applied,
            (1..=15).collect::<Vec<_>>(),
            "each keystroke applied exactly once, in order"
        );
        let bytes: Vec<u8> = st
            .applied_log
            .iter()
            .flat_map(|(_, _, d)| d.clone())
            .collect();
        let expected: String = (1..=15).map(|i| format!("{i};")).collect();
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);
        // Acked input (1..=4) was never resent.
        for seq in 1..=4u64 {
            let n = st.received.iter().filter(|(_, s)| *s == seq).count();
            assert_eq!(n, 1, "seq {seq} received {n} times");
        }
    }
    s.close().await;
}

#[tokio::test]
async fn session_resume_after_drop_quic() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _) = quic_dialer(&daemon);
    resume_after_drop(dialer, daemon, key, cid).await;
}

#[tokio::test]
async fn session_resume_after_drop_stream() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    resume_after_drop(dialer, daemon, key, cid).await;
}

/// Resume refused: unacked input is discarded and numbering restarts at 1.
#[tokio::test]
async fn session_fresh_session_never_resends_input() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _) = quic_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;

    daemon.reject_resume.store(true, Ordering::SeqCst);
    *daemon.drop_at_seq.lock().unwrap() = Some(3);
    for i in 1..=6u64 {
        s.send_input(PaneId(1), vec![i as u8]);
    }
    let c = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    assert!(matches!(
        c,
        SessionEvent::Connected {
            resumed: false,
            instance_changed: false,
            ..
        }
    ));
    // Re-subscribed automatically (Full repaint on the new connection).
    let full = next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    assert!(matches!(full, SessionEvent::Screen(ref u) if u.kind == UpdateKind::Full));
    assert_eq!(
        s.send_input(PaneId(1), b"new".to_vec()),
        1,
        "numbering restarts"
    );
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 1, .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let logged: Vec<(u64, Vec<u8>)> = daemon.clients.lock().unwrap()[&cid]
        .applied_log
        .iter()
        .map(|(_, s, d)| (*s, d.clone()))
        .collect();
    // First session applied 1 and 2; the fresh session applied only "new".
    assert_eq!(
        logged,
        vec![(1, vec![1]), (2, vec![2]), (1, b"new".to_vec())]
    );
    s.close().await;
}

/// A silently dead link: no pong → Recovering → rebind → racing second dial
/// wins (with the resume token) and the old connection is replaced.
#[tokio::test]
async fn session_stall_races_second_connection() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    s.send_input(PaneId(1), b"a".to_vec());
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 1, .. })
    })
    .await;

    daemon.mute_current();
    let t0 = Instant::now();
    // Typed while the link is dead: must arrive exactly once after the race.
    s.send_input(PaneId(1), b"b".to_vec());
    let rec = next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Recovering { .. }))
    })
    .await;
    assert!(matches!(
        rec,
        SessionEvent::Link(LinkState::Recovering { .. })
    ));
    let c = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    assert!(matches!(c, SessionEvent::Connected { resumed: true, .. }));
    let took = t0.elapsed();
    // heartbeat (100) + stall (150) + race (300) + slack
    assert!(
        took >= Duration::from_millis(400) && took < Duration::from_secs(3),
        "{took:?}"
    );
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::InputAcked { seq: 2, .. })
    })
    .await;
    assert!(matches!(
        *s.link_state().borrow(),
        LinkState::Connected { .. }
    ));
    assert_eq!(daemon.connections.load(Ordering::SeqCst), 2);
    let applied: Vec<u64> = daemon.clients.lock().unwrap()[&cid]
        .applied_log
        .iter()
        .map(|(_, s, _)| *s)
        .collect();
    assert_eq!(applied, vec![1, 2]);
    s.close().await;
}

/// The stalled connection comes back to life just as the daemon accepts the
/// session's own race dial (and closes the stalled connection as replaced).
/// The session must end up on the dialed connection: its own dial is never
/// read as another client taking over, however often this happens.
#[tokio::test]
async fn session_own_race_dial_is_not_a_takeover() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _client) = quic_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    *daemon.wake_old_on_replace.lock().unwrap() = Some(Duration::from_millis(100));

    // Twice within REPLACED_WINDOW: the second one used to end the session.
    for round in 1..=2u64 {
        daemon.mute_current();
        let e = next_event(&mut ev, |e| {
            matches!(
                e,
                SessionEvent::Connected { .. } | SessionEvent::Link(LinkState::Failed { .. })
            )
        })
        .await;
        assert!(
            matches!(e, SessionEvent::Connected { resumed: true, .. }),
            "round {round}: {e:?}"
        );
        s.send_input(PaneId(1), vec![b'a']);
        next_event(
            &mut ev,
            |e| matches!(e, SessionEvent::InputAcked { seq, .. } if *seq == round),
        )
        .await;
        // The daemon saw exactly one dial per round: no extra redial after
        // a "replaced" close.
        assert_eq!(daemon.hellos.load(Ordering::SeqCst), 1 + round);
    }
    assert!(matches!(
        *s.link_state().borrow(),
        LinkState::Connected { .. }
    ));
    s.close().await;
}

/// A dial of the session's own that the dialer gave up on (a timeout, the
/// loser of a failover race) after the daemon had accepted its hello: the
/// daemon closes the live connection as replaced *by a resume*, which the
/// session reads as its own doing. It redials at once and never fails with
/// `REPLACED_REASON`.
#[tokio::test]
async fn session_abandoned_dial_is_not_a_takeover() {
    struct Abandoning {
        inner: Arc<QuicDialer>,
        abandon: AtomicBool,
    }
    impl Dialer for Abandoning {
        fn dial(
            &self,
            hello: ClientHello,
            pipelined: Vec<ClientMessage>,
        ) -> BoxFuture<'_, Result<Connected, TransportError>> {
            Box::pin(async move {
                let c = self.inner.dial(hello, pipelined).await?;
                if self.abandon.swap(false, Ordering::SeqCst) {
                    drop(c);
                    return Err(TransportError::Timeout("test: abandoned"));
                }
                Ok(c)
            })
        }
        fn kind(&self) -> TransportKind {
            TransportKind::Quic
        }
    }

    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (inner, _client) = quic_dialer(&daemon);
    let dialer = Arc::new(Abandoning {
        inner,
        abandon: AtomicBool::new(false),
    });
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer.clone()).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    // The "replaced" close arrives after the abandoned dial has ended.
    *daemon.close_replaced_after.lock().unwrap() = Some(Duration::from_millis(100));

    for round in 1..=2u64 {
        dialer.abandon.store(true, Ordering::SeqCst);
        daemon.mute_current();
        let e = next_event(&mut ev, |e| {
            matches!(
                e,
                SessionEvent::Connected { .. } | SessionEvent::Link(LinkState::Failed { .. })
            )
        })
        .await;
        assert!(
            matches!(e, SessionEvent::Connected { resumed: true, .. }),
            "round {round}: {e:?}"
        );
        // The abandoned dial and the redial that followed the close.
        assert_eq!(daemon.hellos.load(Ordering::SeqCst), 1 + 2 * round);
        s.send_input(PaneId(1), vec![b'a']);
        next_event(
            &mut ev,
            |e| matches!(e, SessionEvent::InputAcked { seq, .. } if *seq == round),
        )
        .await;
    }
    s.close().await;
}

/// A stream link whose bytes take `one_way` in each direction.
fn slow_stream_dialer(daemon: &Arc<TestDaemon>, one_way: Duration) -> Arc<dyn Dialer> {
    async fn delay_pipe(
        mut r: impl AsyncRead + Unpin,
        mut w: impl AsyncWrite + Unpin + Send + 'static,
        delay: Duration,
    ) {
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
        let writer = tokio::spawn(async move {
            while let Some((at, bytes)) = rx.recv().await {
                tokio::time::sleep_until(at).await;
                if w.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let mut buf = vec![0u8; 16 * 1024];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0
                || tx
                    .send((tokio::time::Instant::now() + delay, buf[..n].to_vec()))
                    .is_err()
            {
                break;
            }
        }
        drop(tx);
        let _ = writer.await;
    }

    let d = daemon.clone();
    Arc::new(StreamDialer {
        factory: move || {
            let d = d.clone();
            async move {
                let (client, client_far) = tokio::io::duplex(64 * 1024);
                let (daemon_far, server) = tokio::io::duplex(64 * 1024);
                tokio::spawn(d.serve_stream(server));
                let (cr, cw) = tokio::io::split(client_far);
                let (dr, dw) = tokio::io::split(daemon_far);
                tokio::spawn(delay_pipe(cr, dw, one_way));
                tokio::spawn(delay_pipe(dr, cw, one_way));
                Ok::<_, std::io::Error>(client)
            }
        },
    })
}

/// A link whose round trip (200 ms) is longer than the configured stall
/// floor (150 ms) is slow, not stalled: the threshold follows the measured
/// RTT, so the idle session never reports `Recovering` and never redials.
/// A real outage is still detected and recovered.
#[tokio::test]
async fn session_high_latency_link_is_not_a_stall() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = slow_stream_dialer(&daemon, Duration::from_millis(100));
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;

    // Several heartbeat rounds, each ping answered after 200 ms.
    tokio::time::sleep(Duration::from_secs(2)).await;
    while let Ok(e) = ev.try_recv() {
        assert!(
            !matches!(e, SessionEvent::Link(LinkState::Recovering { .. })),
            "slow link reported as stalled: {e:?}"
        );
    }
    assert_eq!(daemon.connections.load(Ordering::SeqCst), 1);
    assert!(matches!(
        *s.link_state().borrow(),
        LinkState::Connected { rtt_ms: Some(rtt), .. } if rtt >= 190
    ));

    // The link dies: detected (later than on a fast link) and raced.
    daemon.mute_current();
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Recovering { .. }))
    })
    .await;
    let c = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    assert!(matches!(c, SessionEvent::Connected { resumed: true, .. }));
    assert_eq!(daemon.connections.load(Ordering::SeqCst), 2);
    s.close().await;
}

/// Anything received counts as proof of life, not only the pong: with pings
/// going unanswered but responses flowing the link is not stalled; once
/// they stop it is.
#[tokio::test]
async fn session_any_traffic_proves_the_link_alive() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    daemon.ignore_pings.store(true, Ordering::SeqCst);

    // 1 s of traffic (the stall floor is 150 ms) with a ping outstanding.
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        s.request(Request::ListRepos).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    while let Ok(e) = ev.try_recv() {
        assert!(
            !matches!(e, SessionEvent::Link(LinkState::Recovering { .. })),
            "live link reported as stalled: {e:?}"
        );
    }
    assert_eq!(daemon.connections.load(Ordering::SeqCst), 1);

    // Silence: now the unanswered ping means a stall.
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Recovering { .. }))
    })
    .await;
    s.close().await;
}

/// A Diff whose base does not match is dropped and a Full is forced.
#[tokio::test]
async fn session_diff_mismatch_forces_full() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _) = quic_dialer(&daemon);
    let (s, mut ev) = start(session_config(key, cid), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;

    daemon.bogus_diff_next.store(true, Ordering::SeqCst);
    s.send_input(PaneId(1), b"x".to_vec());
    let e = next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    assert!(
        matches!(e, SessionEvent::Screen(ref u) if u.kind == UpdateKind::Full),
        "bogus diff must not be delivered: {e:?}"
    );
    let reqs = daemon.subscribe_requests.lock().unwrap().clone();
    assert_eq!(reqs, vec![(PaneId(1), None), (PaneId(1), None)]);
    // Normal diffs flow again afterwards.
    s.send_input(PaneId(1), b"y".to_vec());
    let e = next_event(&mut ev, |e| matches!(e, SessionEvent::Screen(_))).await;
    assert!(matches!(e, SessionEvent::Screen(ref u) if u.kind == UpdateKind::Diff));
    s.close().await;
}

/// Daemon restarted (new instance id): panes are void.
#[tokio::test]
async fn session_detects_daemon_restart() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    s.subscribe(PaneId(1)).await.unwrap();
    daemon.instance_id.store(99, Ordering::SeqCst);
    daemon.drop_all();
    let c = next_event(&mut ev, |e| matches!(e, SessionEvent::Connected { .. })).await;
    assert!(matches!(
        c,
        SessionEvent::Connected {
            instance_changed: true,
            ..
        }
    ));
    assert_eq!(s.server_hello().instance_id, 99);
    // Pane state was cleared: input numbering starts over.
    assert_eq!(s.send_input(PaneId(1), b"z".to_vec()), 1);
    s.close().await;
}

/// Daemon restarted with a new session key: reconnect gets BadKey →
/// Failed { needs_bootstrap: true }.
#[tokio::test]
async fn session_bad_key_on_reconnect_needs_bootstrap() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let dialer = stream_dialer(&daemon);
    let (s, mut ev) = start(fast(session_config(key, cid)), dialer).await;
    *daemon.key.lock().unwrap() = SessionKey::generate().unwrap();
    daemon.drop_all();
    let e = next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Failed { .. }))
    })
    .await;
    assert!(
        matches!(
            e,
            SessionEvent::Link(LinkState::Failed {
                needs_bootstrap: true,
                ..
            })
        ),
        "{e:?}"
    );
    assert!(matches!(
        s.request(Request::ListPanes).await,
        Err(civet_transport::RequestError::Closed)
    ));
}

/// The first connect does not retry, so the caller can fall back.
#[tokio::test]
async fn session_first_connect_times_out_without_daemon() {
    let (key, cid) = hello_template();
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); // nothing answers
    let addr = sock.local_addr().unwrap();
    let client = Arc::new(QuicClient::new(false).unwrap());
    let mut config = QuicClientConfig::new(addr, civet_proto::CertFingerprint([0; 32]));
    config.connect_timeout = Duration::from_millis(300);
    let t0 = Instant::now();
    let err = ClientSession::connect(
        session_config(key, cid),
        Arc::new(QuicDialer { client, config }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TransportError::Timeout(_)), "{err:?}");
    assert!(t0.elapsed() < Duration::from_secs(2));
    let _ = TransportKind::Quic;
}

/// A daemon that predates port forwarding drops the new streams: the client
/// reports `Unsupported` (or the watch ends before its first update) and the
/// connection keeps working.
#[tokio::test]
async fn old_daemon_drops_forward_and_ports_streams_harmlessly() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (dialer, _) = quic_dialer(&daemon);
    let (s, _ev) = start(fast(session_config(key, cid)), dialer).await;

    match s.open_forward(8080).await {
        Err(civet_transport::RequestError::Rpc(e)) => {
            assert_eq!(e.code, civet_proto::ErrorCode::Unsupported, "{e}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    let mut watch = s.watch_ports().await.unwrap();
    let first = tokio::time::timeout(T, watch.next()).await.unwrap();
    assert!(
        !matches!(first, Ok(Some(_))),
        "no update from an old daemon: {first:?}"
    );
    // The connection is unaffected.
    assert!(matches!(
        s.request(Request::Ping { nonce: 9 }).await.unwrap(),
        Response::Pong { nonce: 9, .. }
    ));
    s.close().await;
}
