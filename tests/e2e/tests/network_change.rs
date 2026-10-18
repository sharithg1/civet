//! Scenario 1: network change resume.
//!
//! Through the impairment proxy, N times each (default 20,
//! `CIVET_E2E_RUNS`), at two RTTs (0 ms and 100 ms; `CIVET_E2E_RTTS_MS`
//! as a comma list):
//!
//! * **client rebind**: `notify_network_change()` moves the client to a new
//!   UDP socket (Wi-Fi switch / wake on another network), then a keystroke;
//! * **NAT rebinding**: the proxy's daemon-facing socket is swapped (the
//!   daemon sees the client at a new source port without the client
//!   knowing), then a keystroke.
//!
//! Measures keystroke → echo on the client's screen right after the change
//! (plan target: network back → live terminal < 200 ms) and checks that it
//! was a migration, not a reconnect.

use std::time::Duration;

use civet_e2e::*;
use civet_proto::ClientId;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_change_resume() {
    let _serial = serial().await;
    init_logging();
    let runs: usize = env_num("CIVET_E2E_RUNS", 20);
    let rtts: Vec<u64> = std::env::var("CIVET_E2E_RTTS_MS")
        .unwrap_or_else(|_| "0,100".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let mut failures = Vec::new();
    for rtt in rtts {
        let rtt_d = Duration::from_millis(rtt);
        let daemon = DaemonGuard::start().await;
        let info = daemon.info();
        let proxy = UdpProxy::start(daemon.quic_addr(), Impairment::rtt(rtt_d))
            .await
            .unwrap();
        let (quic, dialer) =
            quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(5));
        let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
            .await
            .unwrap();
        c.transcripts = false;
        let pane = c.spawn(sh_pane(daemon.dir.path())).await;
        c.attach(pane).await;
        c.echo_rtt(pane, "warmup", WAIT).await;
        let baseline: Vec<Duration> = {
            let mut v = Vec::new();
            for i in 0..5 {
                v.push(c.echo_rtt(pane, &format!("base{i}"), WAIT).await);
            }
            v
        };

        // A: the client moves to a new UDP socket.
        let mut client_rebind = Vec::new();
        for i in 0..runs {
            let before = quic.local_addr().unwrap();
            c.session.notify_network_change();
            let d = c.echo_rtt(pane, &format!("cr{i}"), WAIT).await;
            assert_ne!(
                quic.local_addr().unwrap(),
                before,
                "rebind must change the socket"
            );
            client_rebind.push(d);
            c.pump_for(Duration::from_millis(150)).await;
        }
        // B: NAT rebinding: the daemon sees a new source port.
        let mut nat_rebind = Vec::new();
        for i in 0..runs {
            proxy.rebind_upstream().unwrap();
            let d = c.echo_rtt(pane, &format!("nat{i}"), WAIT).await;
            nat_rebind.push(d);
            c.pump_for(Duration::from_millis(150)).await;
        }

        let base = Summary::of(&baseline);
        let a = Summary::of(&client_rebind);
        let b = Summary::of(&nat_rebind);
        eprintln!(
            "rtt {rtt} ms: baseline echo p50 {:.1}; client rebind p50 {:.1} p95 {:.1} max {:.1}; \
             NAT rebind p50 {:.1} p95 {:.1} max {:.1}; reconnects {}",
            base.p50,
            a.p50,
            a.p95,
            a.max,
            b.p50,
            b.p95,
            b.max,
            c.reconnect_count()
        );
        let extra = serde_json::json!({ "rtt_ms": rtt });
        record(
            "network_change.baseline_echo.p50",
            base.p50,
            "ms",
            extra.clone(),
        );
        record_summary(
            &format!("network_change.client_rebind.rtt{rtt}"),
            &a,
            Some(200.0),
        );
        record_summary(
            &format!("network_change.nat_rebind.rtt{rtt}"),
            &b,
            Some(200.0),
        );
        record(
            &format!("network_change.reconnects.rtt{rtt}"),
            c.reconnect_count() as f64,
            "count",
            extra,
        );
        assert!(
            proxy
                .stats()
                .client_addr_changes
                .load(std::sync::atomic::Ordering::Relaxed)
                >= runs as u64,
            "the proxy must have seen the client move"
        );
        assert_eq!(
            c.reconnect_count(),
            0,
            "a network change must migrate, not reconnect"
        );
        // Budgets hold the median: it shows what a change costs (1 RTT for a
        // client rebind, 2 for a NAT rebinding, below) and is steady on
        // shared CI runners, where one or two of the samples can catch a
        // scheduler stall or an extra RTT and move the p95 past a tight
        // budget. The tail is bounded by `max`, loosely: a stall, not a
        // reconnect (counted above). p95 is recorded and noted.
        if a.p50 >= 200.0 {
            failures.push(format!(
                "rtt {rtt}: client rebind p50 {:.1} ms >= 200",
                a.p50
            ));
        }
        // A silent NAT rebinding costs one extra RTT: the daemon receives a
        // single small keystroke datagram on an unvalidated path, so QUIC's
        // anti-amplification limit (3x bytes received, RFC 9000 section 8)
        // holds the echo until PATH_RESPONSE arrives. With the 2 s client
        // keep-alive this only hits a keystroke sent right after a rebind;
        // a wake is the client-rebind case above (1 RTT).
        let nat_budget = 200f64.max(2.0 * rtt as f64 + 60.0);
        if b.p50 >= nat_budget {
            failures.push(format!(
                "rtt {rtt}: NAT rebind p50 {:.1} ms >= {nat_budget} (2 RTT + 60)",
                b.p50
            ));
        }
        let tail_budget = 3.0 * rtt as f64 + 300.0;
        for (what, s) in [("client rebind", &a), ("NAT rebind", &b)] {
            if s.max >= tail_budget {
                failures.push(format!(
                    "rtt {rtt}: {what} max {:.1} ms >= {tail_budget} (3 RTT + 300)",
                    s.max
                ));
            }
            if s.p95 >= 200.0 {
                eprintln!(
                    "NOTE rtt {rtt}: {what} p95 {:.1} ms misses the 200 ms plan target",
                    s.p95
                );
            }
        }
        c.session.close().await;
        daemon.shutdown().await;
    }
    assert!(failures.is_empty(), "{failures:?}");
}
