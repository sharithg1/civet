//! Scenario 5: UDP blocked. The proxy drops every datagram, so the QUIC
//! dial times out (3 s, as the desktop's "auto" transport does) and the
//! client falls back to the stdio tunnel: the real `civetd stdio-tunnel`
//! binary (locally, standing in for `ssh host civetd stdio-tunnel`).
//!
//! Then: the pane works (echo, output, scrollback paging), and when the
//! tunnel process dies (ssh dropped) the session re-dials a new tunnel,
//! resumes, and loses nothing printed meanwhile.

use std::time::Duration;

use civet_e2e::*;
use civet_proto::ClientId;
use civet_transport::{ClientSession, TransportError, TransportKind};
use tokio::time::Instant;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_blocked_falls_back_to_stdio_tunnel() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(
        daemon.quic_addr(),
        Impairment {
            blackhole: true,
            ..Impairment::default()
        },
    )
    .await
    .unwrap();
    let cid = ClientId::generate().unwrap();
    let _ = civetd_bin(); // build it outside the measurement

    let t0 = Instant::now();
    let (_quic, qd) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(3));
    let err = ClientSession::connect(session_config(&info.session_key, cid), qd)
        .await
        .expect_err("QUIC must fail when UDP is blocked");
    assert!(matches!(err, TransportError::Timeout(_)), "{err:?}");
    let quic_gave_up = t0.elapsed();
    let (td, tunnel_pids) = tunnel_dialer(daemon.dir.path());
    let mut c = Client::connect(td, &info.session_key, cid).await.unwrap();
    let connected = t0.elapsed();
    assert!(matches!(
        *c.session.link_state().borrow(),
        civet_transport::LinkState::Connected {
            transport: TransportKind::Stream,
            ..
        }
    ));
    let pane = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(pane).await;
    c.echo_rtt(pane, "via-tunnel", WAIT).await;
    let live = t0.elapsed();
    let mut lat = Vec::new();
    for i in 0..10 {
        lat.push(c.echo_rtt(pane, &format!("t{i}"), WAIT).await);
    }
    let s = Summary::of(&lat);
    let (up, upd, _, _) = proxy.stats().totals();
    assert!(
        up > 0 && up == upd,
        "QUIC packets were sent and all dropped"
    );

    // Output + scrollback over the tunnel.
    c.session
        .send_input(pane, b"seq 1 300 | sed 's/^/u /'; echo TUNNEL-1\n".to_vec());
    c.wait_until(WAIT, "TUNNEL-1", |c| c.shows(pane, "TUNNEL-1"))
        .await;

    // The tunnel process dies (ssh connection dropped) while output continues.
    c.session.send_input(
        pane,
        b"sleep 1; seq 301 900 | sed 's/^/u /'; echo TUNNEL-2\n".to_vec(),
    );
    c.wait_until(WAIT, "command echoed", |c| {
        c.m(pane).rows().iter().any(|r| r.contains("echo TUNNEL-2"))
    })
    .await;
    let first_pid = tunnel_pids.lock().unwrap()[0];
    assert!(signal(first_pid, "KILL"));
    let killed = Instant::now();
    c.wait_until(WAIT, "re-dial", |c| !c.reconnects.is_empty())
        .await;
    let recovered = killed.elapsed();
    c.wait_until(WAIT, "TUNNEL-2 after tunnel restart", |c| {
        c.shows(pane, "TUNNEL-2")
    })
    .await;
    assert_eq!(tunnel_pids.lock().unwrap().len(), 2, "one re-dial");
    assert!(
        c.reconnects.iter().all(|(resumed, _)| *resumed) && !c.reconnects.is_empty(),
        "the tunnel re-dial must resume: {:?}",
        c.reconnects
    );
    c.converge(pane, || daemon.screen(pane), WAIT).await;
    let (history, fetched) = c.full_history(pane).await;
    let run1: Vec<String> = (1..=300).map(|i| format!("u {i}")).collect();
    let run2: Vec<String> = (301..=900).map(|i| format!("u {i}")).collect();
    assert_contiguous(&history, &run1);
    assert_contiguous(&history, &run2);
    // Paging upward with Before works over the tunnel too.
    let page = c
        .session
        .fetch_scrollback(
            pane,
            civet_proto::ScrollbackQuery::Before {
                end: None,
                limit: 50,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.lines.len(), 50);

    eprintln!(
        "UDP blocked: QUIC gave up after {:.0} ms, tunnel connected at {:.0} ms, first echo at {:.0} ms; \
         tunnel echo p50 {:.1} p95 {:.1} ms; tunnel killed -> resumed session in {:.0} ms ({fetched} lines fetched)",
        ms(quic_gave_up),
        ms(connected),
        ms(live),
        s.p50,
        s.p95,
        ms(recovered)
    );
    record(
        "udp_blocked.fallback_live",
        ms(live),
        "ms",
        serde_json::json!({ "quic_timeout_ms": 3000 }),
    );
    record_summary("udp_blocked.tunnel_echo", &s, None);
    record(
        "udp_blocked.tunnel_restart_resume",
        ms(recovered),
        "ms",
        serde_json::json!({}),
    );
    c.session.close().await;
    daemon.shutdown().await;
}
