//! Scenario 4: client restart. The client goes away without a Goodbye
//! (app crash / quit), the pane keeps printing while nobody is connected,
//! then a new client process connects.
//!
//! * `restart_with_resume_token`: the restarted client presented the
//!   persisted resume token, client id and last applied state (new UDP
//!   endpoint, new connection). It must get a `Diff` from exactly that
//!   state, not a Full, and its history must be contiguous.
//!   `ClientSession` has no API to start from a persisted token, so this
//!   drives the connection by hand (`RawClient`), as the session would.
//! * `restart_fresh_session_loses_nothing`: what `ClientSession` /
//!   civet-desktop do today: a new session (same client id, no token) gets
//!   a Full repaint that replays the newest 1000 lines; everything older is
//!   fetched with `fetch_scrollback`. Nothing printed while the client was
//!   gone may be missing.

use std::time::Duration;

use civet_e2e::*;
use civet_proto::{
    ClientId, ClientMessage, ControlRequest, Request, Response, ServerMessage, UpdateKind,
};
use civet_transport::QuicClient;
use tokio::time::Instant;

const LINES: u32 = 1500;

async fn wait_server_row(daemon: &DaemonGuard, pane: civet_proto::PaneId, row: &str) {
    let deadline = Instant::now() + WAIT;
    while !daemon.screen(pane).iter().any(|r| r == row) {
        assert!(Instant::now() < deadline, "pane never printed {row}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_with_resume_token() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let cfg = civet_transport::QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint);
    let cid = ClientId::generate().unwrap();

    // First "process".
    let endpoint1 = QuicClient::new(false).unwrap();
    let mut c1 = RawClient::new(
        endpoint1
            .connect(&cfg, raw_hello(&info.session_key, cid), vec![])
            .await
            .unwrap(),
    );
    let pane = match c1.request(sh_pane(daemon.dir.path())).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    c1.request(Request::SubscribePane {
        pane,
        known_state: None,
    })
    .await
    .unwrap();
    let send = |c: &RawClient, seq: u64, s: String| {
        let conn = c.conn.clone();
        async move {
            conn.send(ClientMessage::Input(civet_proto::InputMessage {
                pane,
                seq,
                data: s.into_bytes(),
            }))
            .await
            .unwrap();
        }
    };
    send(&c1, 1, "echo start\n".into()).await;
    c1.wait_screen(pane, |m| m.has_row("start")).await;
    send(
        &c1,
        2,
        format!("sleep 1; seq 1 {LINES} | sed 's/^/r /'; echo RESTART-DONE\n"),
    )
    .await;
    c1.wait_screen(pane, |m| {
        m.rows().iter().any(|r| r.contains("RESTART-DONE"))
    })
    .await;
    // Persisted by the client before it died.
    let token = c1.hello.resume_token.clone();
    let known = c1.mirrors[&pane].state;
    let mut mirror = c1.mirrors.remove(&pane).unwrap();
    assert!(!mirror.has_row("r 1"));
    drop(c1);
    drop(endpoint1); // the process is gone

    wait_server_row(&daemon, pane, "RESTART-DONE").await;

    // Second "process": new endpoint, resume token.
    let t0 = Instant::now();
    let endpoint2 = QuicClient::new(false).unwrap();
    let resub = ClientMessage::Request(ControlRequest {
        id: 1,
        request: Request::SubscribePane {
            pane,
            known_state: Some(known),
        },
    });
    let mut c2 = RawClient::new(
        endpoint2
            .connect(
                &cfg,
                raw_hello(&info.session_key, cid).with_resume(Some(token)),
                vec![resub],
            )
            .await
            .unwrap(),
    );
    assert!(
        c2.hello.resumed,
        "the persisted resume token must be accepted"
    );
    assert_eq!(c2.hello.resumed_subscriptions.len(), 1);
    assert_eq!(c2.hello.resumed_subscriptions[0].last_input_seq, 2);
    let first = loop {
        match tokio::time::timeout(WAIT, c2.rx.recv()).await {
            Ok(Ok(Some(ServerMessage::Screen(u)))) => break u,
            Ok(Ok(Some(_))) => {}
            other => panic!("no screen update after resume: {other:?}"),
        }
    };
    let first_ms = ms(t0.elapsed());
    assert_eq!(
        first.kind,
        UpdateKind::Diff,
        "resume must continue with a Diff"
    );
    assert_eq!(first.base_state, known);
    mirror.apply(&first);
    c2.mirrors.insert(pane, mirror);
    c2.wait_screen(pane, |m| m.has_row("RESTART-DONE")).await;
    let m = &c2.mirrors[&pane];
    assert_eq!(m.fulls, 1, "only the initial Full");
    // History: transcript + any gap fetched + screen.
    let mut t = m.transcript.clone().unwrap();
    for (s, e) in m.missing_ranges() {
        for (i, l) in c2.fetch(pane, s, e).await.into_iter().enumerate() {
            t.insert(s + i as u64, l);
        }
    }
    let mut lines: Vec<String> = t.into_values().collect();
    lines.extend(trim_rows(&m.rows()));
    let expected: Vec<String> = (1..=LINES).map(|i| format!("r {i}")).collect();
    assert_contiguous(&lines, &expected);
    eprintln!(
        "client restart with resume token: first update {first_ms:.1} ms after dial \
         ({} B, history {}..{}), gaps {:?}",
        first.payload.len(),
        first.history_start,
        first.history_lines,
        m.gaps
    );
    record(
        "client_restart.resume_token.first_update",
        first_ms,
        "ms",
        serde_json::json!({}),
    );
    c2.conn.close().await;
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_fresh_session_loses_nothing() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let cid = ClientId::generate().unwrap();
    let (_q1, d1) = quic_dialer(
        daemon.quic_addr(),
        info.cert_fingerprint,
        Duration::from_secs(5),
    );
    let mut c1 = Client::connect(d1, &info.session_key, cid).await.unwrap();
    let pane = c1.spawn(sh_pane(daemon.dir.path())).await;
    c1.attach(pane).await;
    c1.echo_rtt(pane, "before", WAIT).await;
    c1.session.send_input(
        pane,
        format!("sleep 1; seq 1 {LINES} | sed 's/^/f /'; echo FRESH-DONE\n").into_bytes(),
    );
    c1.wait_until(WAIT, "command echoed", |c| {
        c.m(pane).rows().iter().any(|r| r.contains("FRESH-DONE"))
    })
    .await;
    drop(c1); // no Goodbye: crash
    wait_server_row(&daemon, pane, "FRESH-DONE").await;

    let t0 = Instant::now();
    let (_q2, d2) = quic_dialer(
        daemon.quic_addr(),
        info.cert_fingerprint,
        Duration::from_secs(5),
    );
    let mut c2 = Client::connect(d2, &info.session_key, cid).await.unwrap();
    assert!(!c2.first_hello.resumed);
    c2.attach(pane).await;
    c2.wait_until(WAIT, "repaint", |c| c.shows(pane, "FRESH-DONE"))
        .await;
    let repaint = t0.elapsed();
    c2.converge(pane, || daemon.screen(pane), WAIT).await;
    let (history, fetched) = c2.full_history(pane).await;
    let fetch_done = t0.elapsed();
    let expected: Vec<String> = (1..=LINES).map(|i| format!("f {i}")).collect();
    assert_contiguous(&history, &expected);
    assert!(history.iter().any(|l| l == "before"));
    assert!(
        fetched > 0,
        "a Full replays at most 1000 lines; the rest must be fetched"
    );
    eprintln!(
        "client restart (fresh session): repaint {:.1} ms, full history ({fetched} lines fetched) {:.1} ms",
        ms(repaint),
        ms(fetch_done)
    );
    record(
        "client_restart.fresh.repaint",
        ms(repaint),
        "ms",
        serde_json::json!({}),
    );
    record(
        "client_restart.fresh.history_complete",
        ms(fetch_done),
        "ms",
        serde_json::json!({ "fetched": fetched }),
    );
    c2.session.close().await;
    daemon.shutdown().await;
}
