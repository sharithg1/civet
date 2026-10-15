//! ClientSession tests: subscribe/screen/input/scrollback over both links,
//! migration via notify_network_change, resume after a full drop with exact-
//! once input resend, fresh-session semantics, stall → racing re-dial, diff
//! validation, daemon restart detection, and BadKey → Failed.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use civet_proto::{ClientId, PaneId, Request, Response, ScrollbackQuery, SessionKey, UpdateKind};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, LinkState, QuicClient, QuicClientConfig,
    QuicDialer, SessionEvent, StreamDialer, TransportError, TransportKind,
};
use common::{TestDaemon, eventually, hello_template, session_config};
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
    next_event(&mut ev, |e| {
        matches!(e, SessionEvent::Link(LinkState::Connected { .. }))
    })
    .await;
    assert_eq!(daemon.connections.load(Ordering::SeqCst), 2);
    let applied: Vec<u64> = daemon.clients.lock().unwrap()[&cid]
        .applied_log
        .iter()
        .map(|(_, s, _)| *s)
        .collect();
    assert_eq!(applied, vec![1, 2]);
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
