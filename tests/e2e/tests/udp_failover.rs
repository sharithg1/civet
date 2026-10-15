//! Scenario 5b: UDP gets blocked *mid-session* (plan gap H2). The session
//! runs on QUIC through the impairment proxy with a `FailoverDialer` (QUIC
//! first, the real `civetd stdio-tunnel` as fallback), as civet-desktop's
//! "auto" transport and civet-cli do. Then:
//!
//! 1. the proxy blackholes UDP → the session stalls, fails over to the
//!    tunnel, resumes, and the pane keeps working (input typed during the
//!    outage arrives exactly once);
//! 2. UDP is restored → the periodic probe moves the session back to QUIC
//!    on its own, again without losing or duplicating anything.

use std::sync::Arc;
use std::time::Duration;

use civet_e2e::*;
use civet_proto::ClientId;
use civet_transport::{Dialer, FailoverConfig, FailoverDialer, LinkState, TransportKind};
use tokio::time::Instant;

fn on(c: &Client, kind: TransportKind) -> bool {
    matches!(&*c.session.link_state().borrow(), LinkState::Connected { transport, .. } if *transport == kind)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_blocked_mid_session_fails_over_and_back() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(daemon.quic_addr(), Impairment::default())
        .await
        .unwrap();
    let (_quic, qd) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(3));
    let (td, tunnel_pids) = tunnel_dialer(daemon.dir.path());
    let failover = Arc::new(FailoverDialer::new(
        qd,
        td,
        FailoverConfig {
            probe_interval: Duration::from_secs(2),
            ..FailoverConfig::default()
        },
    ));
    let dialer: Arc<dyn Dialer> = failover.clone();
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    assert!(on(&c, TransportKind::Quic));
    let pane = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(pane).await;
    c.echo_rtt(pane, "on-quic", WAIT).await;

    // 1. UDP blackholed mid-session; type while it is down.
    proxy.blackhole(true);
    let t0 = Instant::now();
    c.session
        .send_input(pane, b"echo typed-during-outage\n".to_vec());
    c.wait_until(Duration::from_secs(30), "failover to the tunnel", |c| {
        on(c, TransportKind::Stream)
    })
    .await;
    let failed_over = t0.elapsed();
    assert!(failover.on_fallback());
    assert!(
        !tunnel_pids.lock().unwrap().is_empty(),
        "a tunnel was spawned"
    );
    c.wait_until(WAIT, "outage input", |c| {
        c.shows(pane, "typed-during-outage")
    })
    .await;
    let on_tunnel = c.echo_rtt(pane, "via-tunnel", WAIT).await;
    assert!(
        c.reconnects.iter().all(|(resumed, _)| *resumed),
        "failover resumes the session: {:?}",
        c.reconnects
    );

    // 2. UDP back: the probe moves the session back to QUIC.
    proxy.blackhole(false);
    let t1 = Instant::now();
    c.wait_until(Duration::from_secs(30), "back on QUIC", |c| {
        on(c, TransportKind::Quic)
    })
    .await;
    let back = t1.elapsed();
    assert!(!failover.on_fallback());
    c.echo_rtt(pane, "back-on-quic", WAIT).await;
    c.converge(pane, || daemon.screen(pane), WAIT).await;
    // Every command ran exactly once: its output line appears once.
    for word in ["typed-during-outage", "via-tunnel", "back-on-quic"] {
        assert_eq!(
            c.m(pane).count_rows(word),
            1,
            "{word}: {:?}",
            c.m(pane).rows()
        );
    }
    assert!(
        c.reconnects.iter().all(|(resumed, _)| *resumed),
        "{:?}",
        c.reconnects
    );
    eprintln!(
        "UDP blocked mid-session: on the tunnel after {:.0} ms (echo there {:.0} ms); \
         UDP restored -> back on QUIC after {:.0} ms",
        ms(failed_over),
        ms(on_tunnel),
        ms(back)
    );
    record(
        "udp_failover.to_tunnel",
        ms(failed_over),
        "ms",
        serde_json::json!({}),
    );
    record(
        "udp_failover.back_to_quic",
        ms(back),
        "ms",
        serde_json::json!({ "probe_interval_ms": 2000 }),
    );
    c.session.close().await;
    daemon.shutdown().await;
}
