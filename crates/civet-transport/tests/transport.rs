//! Connection-level tests: auth, pinning, every stream kind over QUIC and the
//! byte-stream fallback, and migration via `rebind`.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use civet_proto::{
    CertFingerprint, ClientHello, ClientMessage, ControlRequest, ControlResponse, Event,
    InputMessage, PaneId, RejectReason, Request, Response, ScrollbackQuery, ScrollbackRequest,
    ServerMessage, SessionKey, UpdateKind,
};
use civet_transport::{
    ClientReceiver, Connected, QuicClient, QuicClientConfig, TransportError, TransportKind,
    connect_stream,
};
use common::{TestDaemon, eventually, hello_template};

const T: Duration = Duration::from_secs(5);

async fn recv(rx: &mut ClientReceiver) -> ServerMessage {
    tokio::time::timeout(T, rx.recv())
        .await
        .expect("timed out waiting for a server message")
        .expect("receive failed")
        .expect("connection ended")
}

/// Receive until `pred` matches, returning the match and everything skipped.
async fn recv_until(
    rx: &mut ClientReceiver,
    mut pred: impl FnMut(&ServerMessage) -> bool,
) -> (ServerMessage, Vec<ServerMessage>) {
    let mut skipped = Vec::new();
    loop {
        let m = recv(rx).await;
        if pred(&m) {
            return (m, skipped);
        }
        skipped.push(m);
    }
}

fn request(id: u64, request: Request) -> ClientMessage {
    ClientMessage::Request(ControlRequest { id, request })
}

fn is_response(m: &ServerMessage, want: u64) -> bool {
    matches!(m, ServerMessage::Response(ControlResponse { id, .. }) if *id == want)
}

async fn quic_setup() -> (
    Arc<TestDaemon>,
    SocketAddr,
    CertFingerprint,
    SessionKey,
    civet_proto::ClientId,
) {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (server, fp) = daemon.spawn_quic();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_addr().port())
        .parse()
        .unwrap();
    // Keep the server alive for the test's duration.
    std::mem::forget(server);
    (daemon, addr, fp, key, cid)
}

/// Exercise control RPC, events, per-pane screen streams, input, and
/// scrollback; the same code runs over QUIC and the stream link.
async fn exercise_all_streams(c: Connected, daemon: &TestDaemon, cid: civet_proto::ClientId) {
    let Connected {
        conn,
        mut incoming,
        server_hello,
    } = c;
    assert!(!server_hello.resumed);

    // Pipelined subscribe for pane 1 was sent with the hello.
    let (sub1, skipped) = recv_until(&mut incoming, |m| is_response(m, 1)).await;
    assert!(matches!(
        sub1,
        ServerMessage::Response(ControlResponse { result: Ok(Response::Subscribed(ref i)), .. }) if i.pane == PaneId(1)
    ));
    let mut screens: Vec<_> = skipped
        .into_iter()
        .filter_map(|m| match m {
            ServerMessage::Screen(u) => Some(u),
            _ => None,
        })
        .collect();
    if screens.is_empty() {
        match recv(&mut incoming).await {
            ServerMessage::Screen(u) => screens.push(u),
            other => panic!("expected screen, got {other:?}"),
        }
    }
    assert_eq!(screens[0].pane, PaneId(1));
    assert_eq!(screens[0].kind, UpdateKind::Full);

    // Ping / pong on the control stream.
    conn.send(request(2, Request::Ping { nonce: 42 }))
        .await
        .unwrap();
    let (pong, _) = recv_until(&mut incoming, |m| is_response(m, 2)).await;
    assert!(matches!(
        pong,
        ServerMessage::Response(ControlResponse {
            result: Ok(Response::Pong { nonce: 42, .. }),
            ..
        })
    ));

    // An event pushed before the response, on the same stream, in order.
    conn.send(request(3, Request::ListRepos)).await.unwrap();
    let ev = recv(&mut incoming).await;
    assert!(
        matches!(ev, ServerMessage::Event(Event::ReposChanged)),
        "{ev:?}"
    );
    let resp = recv(&mut incoming).await;
    assert!(is_response(&resp, 3));

    // Second pane gets its own screen stream.
    conn.send(request(
        4,
        Request::SubscribePane {
            pane: PaneId(2),
            known_state: None,
        },
    ))
    .await
    .unwrap();
    let (_, skipped) = recv_until(&mut incoming, |m| is_response(m, 4)).await;
    let mut got_full2 = skipped
        .iter()
        .any(|m| matches!(m, ServerMessage::Screen(u) if u.pane == PaneId(2) && u.kind == UpdateKind::Full));
    if !got_full2 {
        let m = recv(&mut incoming).await;
        got_full2 = matches!(m, ServerMessage::Screen(ref u) if u.pane == PaneId(2) && u.kind == UpdateKind::Full);
    }
    assert!(got_full2);

    // Input on both panes: in-order diffs per pane plus input acks.
    for seq in 1..=20u64 {
        for pane in [PaneId(1), PaneId(2)] {
            conn.send(ClientMessage::Input(InputMessage {
                pane,
                seq,
                data: format!("{pane}:{seq}").into_bytes(),
            }))
            .await
            .unwrap();
        }
    }
    let mut last_state = std::collections::HashMap::new();
    let mut acks = std::collections::HashMap::new();
    let mut diffs = 0;
    while acks.values().filter(|s| **s == 20u64).count() < 2 || diffs < 40 {
        match recv(&mut incoming).await {
            ServerMessage::Screen(u) => {
                assert_eq!(u.kind, UpdateKind::Diff);
                let prev = last_state.insert(u.pane, u.state);
                if let Some(prev) = prev {
                    assert_eq!(u.base_state, prev, "per-pane order");
                }
                let expected_seq = u.acked_input_seq;
                assert_eq!(
                    u.payload,
                    format!("{}:{}", u.pane, expected_seq).into_bytes()
                );
                diffs += 1;
            }
            ServerMessage::InputAck { pane, seq } => {
                let prev = acks.insert(pane, seq).unwrap_or(0);
                assert!(seq > prev);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // Scrollback (own bidi stream on QUIC; control RPC on stream links).
    let page = conn
        .fetch_scrollback(ScrollbackRequest {
            req_id: 7,
            pane: PaneId(1),
            query: ScrollbackQuery::Before {
                end: Some(500),
                limit: 100,
            },
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.req_id, 7);
    assert_eq!(page.start, 400);
    assert_eq!(page.lines.len(), 100);
    assert_eq!(page.lines[0].vt, b"line 400");
    // Concurrent scrollback requests are matched correctly.
    let (a, b) = tokio::join!(
        conn.fetch_scrollback(ScrollbackRequest {
            req_id: 8,
            pane: PaneId(1),
            query: ScrollbackQuery::Range { start: 0, end: 3 },
        }),
        conn.fetch_scrollback(ScrollbackRequest {
            req_id: 9,
            pane: PaneId(2),
            query: ScrollbackQuery::Range { start: 10, end: 12 },
        })
    );
    let (a, b) = (a.unwrap().unwrap(), b.unwrap().unwrap());
    assert_eq!((a.req_id, a.lines.len()), (8, 3));
    assert_eq!((b.req_id, b.start, b.lines.len()), (9, 10, 2));

    {
        let c = daemon.client(&cid);
        let st = &c[&cid];
        assert_eq!(st.applied_log.len(), 40);
    }

    // Goodbye reaches the daemon; the receiver ends cleanly.
    conn.close().await;
    assert!(
        eventually(T, || daemon.clients.lock().unwrap()[&cid].goodbye).await,
        "daemon saw goodbye"
    );
    let end = tokio::time::timeout(T, async {
        loop {
            match incoming.recv().await {
                Ok(Some(_)) => continue,
                other => return other,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(end, Ok(None) | Err(TransportError::Closed(_))),
        "{end:?}"
    );
}

#[tokio::test]
async fn quic_auth_success_and_roundtrip_all_streams() {
    let (daemon, addr, fp, key, cid) = quic_setup().await;
    let client = QuicClient::new(false).unwrap();
    let c = client
        .connect(
            &QuicClientConfig::new(addr, fp),
            ClientHello::new(key, cid, "t", "0"),
            vec![request(
                1,
                Request::SubscribePane {
                    pane: PaneId(1),
                    known_state: None,
                },
            )],
        )
        .await
        .expect("connect");
    assert_eq!(c.conn.kind(), TransportKind::Quic);
    assert!(c.conn.rtt().is_some());
    exercise_all_streams(c, &daemon, cid).await;
}

#[tokio::test]
async fn stream_auth_success_and_roundtrip_all_streams() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (a, b) = tokio::io::duplex(1024);
    tokio::spawn(daemon.clone().serve_stream(b));
    let c = connect_stream(
        a,
        ClientHello::new(key, cid, "t", "0"),
        vec![request(
            1,
            Request::SubscribePane {
                pane: PaneId(1),
                known_state: None,
            },
        )],
    )
    .await
    .expect("connect");
    assert_eq!(c.conn.kind(), TransportKind::Stream);
    assert!(c.conn.rtt().is_none());
    c.conn.rebind().unwrap(); // no-op on stream links
    exercise_all_streams(c, &daemon, cid).await;
}

#[tokio::test]
async fn quic_wrong_session_key_is_rejected() {
    let (_daemon, addr, fp, _key, cid) = quic_setup().await;
    let client = QuicClient::new(false).unwrap();
    let err = client
        .connect(
            &QuicClientConfig::new(addr, fp),
            ClientHello::new(SessionKey([0xEE; 32]), cid, "t", "0"),
            vec![request(1, Request::ListPanes)],
        )
        .await
        .unwrap_err();
    match &err {
        TransportError::Rejected(r) => assert_eq!(r.reason, RejectReason::BadKey),
        other => panic!("expected Rejected(BadKey), got {other:?}"),
    }
    assert!(err.is_fatal());
}

#[tokio::test]
async fn stream_wrong_session_key_and_version_are_rejected() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());

    let (a, b) = tokio::io::duplex(1024);
    tokio::spawn(daemon.clone().serve_stream(b));
    let err = connect_stream(
        a,
        ClientHello::new(SessionKey([1; 32]), cid, "t", "0"),
        vec![],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, TransportError::Rejected(r) if r.reason == RejectReason::BadKey),
        "{err:?}"
    );

    let (a, b) = tokio::io::duplex(1024);
    tokio::spawn(daemon.clone().serve_stream(b));
    let mut hello = ClientHello::new(key, cid, "t", "0");
    hello.proto_version = 999;
    let err = connect_stream(a, hello, vec![]).await.unwrap_err();
    assert!(
        matches!(&err, TransportError::Rejected(r) if r.reason == RejectReason::VersionMismatch),
        "{err:?}"
    );
    assert_eq!(
        daemon.connections.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn quic_wrong_fingerprint_is_rejected() {
    let (daemon, addr, fp, key, cid) = quic_setup().await;
    let client = QuicClient::new(false).unwrap();
    let wrong = CertFingerprint([0xAB; 32]);
    let err = client
        .connect(
            &QuicClientConfig::new(addr, wrong),
            ClientHello::new(key, cid, "t", "0"),
            vec![],
        )
        .await
        .unwrap_err();
    match err {
        TransportError::FingerprintMismatch { expected, actual } => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, fp);
        }
        other => panic!("expected FingerprintMismatch, got {other:?}"),
    }
    // The session key never reached the daemon.
    assert_eq!(
        daemon.connections.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn quic_rebind_keeps_connection_alive() {
    let (daemon, addr, fp, key, cid) = quic_setup().await;
    let client = QuicClient::new(false).unwrap();
    let Connected {
        conn, mut incoming, ..
    } = client
        .connect(
            &QuicClientConfig::new(addr, fp),
            ClientHello::new(key, cid, "t", "0"),
            vec![request(
                1,
                Request::SubscribePane {
                    pane: PaneId(1),
                    known_state: None,
                },
            )],
        )
        .await
        .unwrap();
    recv_until(&mut incoming, |m| is_response(m, 1)).await;

    let mut next_id = 10;
    let mut seq = 0;
    let mut timings = Vec::new();
    for round in 0..5 {
        let before_local = client.local_addr().unwrap();
        let before_remote = daemon.remote_addr(&cid).unwrap();
        client.rebind().unwrap();
        let after_local = client.local_addr().unwrap();
        assert_ne!(before_local.port(), after_local.port(), "new UDP socket");

        // Time to first message after rebind: a ping round trip on the
        // migrated path.
        let t0 = Instant::now();
        next_id += 1;
        conn.send(request(next_id, Request::Ping { nonce: round }))
            .await
            .unwrap();
        let id = next_id;
        recv_until(&mut incoming, |m| is_response(m, id)).await;
        let elapsed = t0.elapsed();
        timings.push(elapsed);

        // The daemon now sees the new client address (migration).
        assert!(
            eventually(T, || daemon.remote_addr(&cid).map(|a| a.port())
                == Some(after_local.port()))
            .await,
            "daemon followed the migration ({before_remote} -> {after_local})"
        );

        // Screen stream continues in order across the migration.
        seq += 1;
        conn.send(ClientMessage::Input(InputMessage {
            pane: PaneId(1),
            seq,
            data: b"x".to_vec(),
        }))
        .await
        .unwrap();
        let (m, _) = recv_until(&mut incoming, |m| matches!(m, ServerMessage::Screen(_))).await;
        match m {
            ServerMessage::Screen(u) => assert_eq!(u.acked_input_seq, seq),
            _ => unreachable!(),
        }
    }
    let max = timings.iter().max().unwrap();
    eprintln!(
        "rebind time-to-first-message (ping RTT after Endpoint::rebind): {:?}",
        timings
    );
    assert!(*max < Duration::from_millis(200), "rebind latency {max:?}");
    // Still the same connection: the daemon accepted exactly one.
    assert_eq!(
        daemon.connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn quic_server_close_ends_client() {
    let (key, cid) = hello_template();
    let daemon = TestDaemon::new(key.clone());
    let (server, fp) = daemon.spawn_quic();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_addr().port())
        .parse()
        .unwrap();
    let client = QuicClient::new(false).unwrap();
    let Connected {
        conn, mut incoming, ..
    } = client
        .connect(
            &QuicClientConfig::new(addr, fp),
            ClientHello::new(key, cid, "t", "0"),
            vec![],
        )
        .await
        .unwrap();
    server.close("shutting down");
    let r = tokio::time::timeout(T, incoming.recv()).await.unwrap();
    assert!(r.is_err() || matches!(r, Ok(None)), "{r:?}");
    let e = tokio::time::timeout(T, conn.closed()).await.unwrap();
    assert!(
        matches!(e, TransportError::Closed(ref s) if s.contains("code 4")),
        "{e:?}"
    );
    assert!(server.accept().await.is_none());
}
