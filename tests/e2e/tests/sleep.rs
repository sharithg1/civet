//! Scenario 3: "sleep". The proxy blackholes all traffic for
//! `CIVET_E2E_SLEEP_SECS` (default 10; e.g. 60 or 300 for a long run) while
//! a pane keeps printing a numbered line every 50 ms. Then the network
//! comes back and the client calls `notify_network_change()` (what the
//! desktop's wake detector does).
//!
//! Checks: the session resumes (in place or via a resumed reconnect, never
//! a fresh session), the screen converges to the daemon's, and every line
//! printed during the blackout is in the client's history (replayed, or
//! fetched for a gap) exactly once and in order. Reports time to live:
//! network back → keystroke echoed in a second pane.
//!
//! ```sh
//! CIVET_E2E_SLEEP_SECS=60 cargo test -p civet-e2e --release --test sleep -- --nocapture
//! ```

use std::time::Duration;

use civet_e2e::*;
use civet_proto::ClientId;
use civet_transport::LinkState;
use tokio::time::Instant;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleep_blackhole_resume() {
    let _serial = serial().await;
    init_logging();
    let sleep = Duration::from_secs(env_num("CIVET_E2E_SLEEP_SECS", 10));
    let rtt = Duration::from_millis(env_num("CIVET_E2E_RTT_MS", 50));
    // Enough lines to keep printing through the blackout (sleep 0.05 per line).
    let n = (sleep.as_secs() + 8) * 20;

    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(daemon.quic_addr(), Impairment::rtt(rtt))
        .await
        .unwrap();
    let (_quic, dialer) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(5));
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    let producer = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(producer).await;
    let probe = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(probe).await;
    c.echo_rtt(probe, "ready", WAIT).await;
    // `set +m`: no job control, so the shell does not setpgid each `sleep`;
    // macOS's sh (bash 3.2) can lose that race and print "child setpgid …
    // Operation not permitted" into the run of ticks.
    c.session.send_input(
        producer,
        format!(
            "set +m; i=0; while [ $i -lt {n} ]; do i=$((i+1)); echo \"tick $i\"; sleep 0.05; done; echo PRODUCER-DONE\n"
        )
        .into_bytes(),
    );
    c.wait_until(WAIT, "first ticks", |c| c.shows(producer, "tick 20"))
        .await;

    // Sleep.
    proxy.blackhole(true);
    let slept = Instant::now();
    c.pump_for(sleep).await;
    let ticks_at_sleep_end = daemon
        .screen(producer)
        .iter()
        .filter_map(|r| r.strip_prefix("tick ").and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    let went_recovering = c
        .links
        .iter()
        .any(|(t, l)| *t >= slept && matches!(l, LinkState::Recovering { .. }));

    // Wake.
    proxy.blackhole(false);
    let woke = Instant::now();
    c.session.notify_network_change();
    let live = c.echo_rtt(probe, "alive", Duration::from_secs(30)).await;
    let link_ok = c
        .links
        .iter()
        .find(|(t, l)| *t >= woke && matches!(l, LinkState::Connected { .. }))
        .map(|(t, _)| *t - woke);
    // Caught up: the producer's screen matches the daemon's (first time).
    c.converge(
        producer,
        || daemon.screen(producer),
        Duration::from_secs(30),
    )
    .await;
    let caught_up = woke.elapsed();

    c.wait_until(
        Duration::from_secs(sleep.as_secs() + 60),
        "producer done",
        |c| c.shows(producer, "PRODUCER-DONE"),
    )
    .await;
    c.converge(
        producer,
        || daemon.screen(producer),
        Duration::from_secs(30),
    )
    .await;
    let (history, fetched) = c.full_history(producer).await;
    let expected: Vec<String> = (1..=n).map(|i| format!("tick {i}")).collect();
    assert_contiguous(&history, &expected);
    assert!(history.iter().any(|l| l == "PRODUCER-DONE"));
    let m = c.m(producer);
    assert_eq!(m.base_mismatches, 0);
    assert_eq!(m.replay_mismatches, 0);
    assert!(
        c.reconnects
            .iter()
            .all(|(resumed, changed)| *resumed && !changed),
        "every reconnect must be a resume: {:?}",
        c.reconnects
    );

    eprintln!(
        "sleep {sleep:?} (rtt {rtt:?}, ~{} lines printed during it): recovering={went_recovering}; \
         network back -> echo {:.0} ms, link Connected after {:?}, producer caught up after {:.0} ms; \
         reconnects {:?}; gaps {:?}; {fetched} lines fetched; fulls {}",
        ticks_at_sleep_end.saturating_sub(20),
        ms(live),
        link_ok,
        ms(caught_up),
        c.reconnects,
        m.gaps,
        m.fulls
    );
    let extra = serde_json::json!({
        "sleep_s": sleep.as_secs(), "rtt_ms": rtt.as_millis() as u64,
        "reconnects": c.reconnect_count(), "fetched": fetched, "fulls": m.fulls,
        "link_connected_ms": link_ok.map(ms),
    });
    record(
        &format!("sleep{}s.time_to_live", sleep.as_secs()),
        ms(live),
        "ms",
        extra.clone(),
    );
    record(
        &format!("sleep{}s.caught_up", sleep.as_secs()),
        ms(caught_up),
        "ms",
        extra,
    );
    c.session.close().await;
    daemon.shutdown().await;
}
