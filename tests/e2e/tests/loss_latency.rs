//! Scenario 2: packet loss + latency (5% and 20% loss, 100 ms RTT, 10 ms
//! jitter, 2% reordering, both directions).
//!
//! A pane runs `cat > typed.txt`; the test types a known sequence one
//! keystroke per `InputMessage` (more lines than the screen has rows, so
//! the screen scrolls). Then:
//!
//! * the file equals the typed text exactly: no lost, duplicated or
//!   reordered input;
//! * the client's screen converges to the daemon's;
//! * the client's history (replayed lines, gaps filled by
//!   `fetch_scrollback`) + screen is exactly the typed lines, contiguous;
//! * keystroke → echo latency under loss is measured in a second pane.

use std::time::Duration;

use civet_e2e::*;
use civet_proto::{ClientId, PaneId};
use tokio::time::Instant;

fn typed_lines(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            format!(
                "k{i:03} quick brown fox {} jumps {:x}",
                "#".repeat(i % 7),
                i * 7919
            )
        })
        .collect()
}

async fn loss_run(loss: f64) {
    let _serial = serial().await;
    init_logging();
    let rtt = Duration::from_millis(env_num("CIVET_E2E_RTT_MS", 100));
    let lines = typed_lines(env_num("CIVET_E2E_LINES", 40));
    let pct = (loss * 100.0).round() as u32;

    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(daemon.quic_addr(), Impairment::lossy(loss, rtt))
        .await
        .unwrap();
    let (_quic, dialer) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(5));
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    let out = daemon.dir.path().join("typed.txt");
    let pane = c
        .spawn(cmd_pane(
            daemon.dir.path(),
            "sh",
            &["-c", "exec cat > typed.txt"],
        ))
        .await;
    c.attach(pane).await;
    let probe = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(probe).await;
    c.wait_until(WAIT, "initial screens", |c| {
        c.m(pane).state > 0 && !trim_rows(&c.m(probe).rows()).is_empty()
    })
    .await;
    c.echo_rtt(probe, "warmup", WAIT).await;

    // Type: one keystroke per message, in bursts (fast typist / paste-ish).
    let t0 = Instant::now();
    let mut expected = String::new();
    for line in &lines {
        for ch in line.chars().chain(std::iter::once('\r')) {
            c.session.send_input(pane, ch.to_string().into_bytes());
            expected.push(if ch == '\r' { '\n' } else { ch });
            if expected.len() % 8 == 0 {
                c.pump_for(Duration::from_millis(5)).await;
            }
        }
    }
    let keystrokes = expected.len();
    // The file has exactly what was typed (cat writes each line as it gets it).
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let got = std::fs::read_to_string(&out).unwrap_or_default();
        if got.len() >= expected.len() {
            assert_eq!(
                got, expected,
                "typed text must arrive exactly once, in order"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "input never fully arrived: {} of {} bytes",
            got.len(),
            expected.len()
        );
        c.pump_for(Duration::from_millis(50)).await;
    }
    let input_done = t0.elapsed();
    // Screen converges; transcript is exactly the typed lines.
    c.converge(pane, || daemon.screen(pane), Duration::from_secs(60))
        .await;
    let converged = t0.elapsed();
    let (history, fetched) = c.full_history(pane).await;
    let history: Vec<String> = history.into_iter().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        history, lines,
        "client history + screen must be exactly the typed lines"
    );
    assert_eq!(c.m(pane).base_mismatches, 0);
    assert_eq!(c.m(pane).replay_mismatches, 0);

    // Keystroke -> echo under loss, in the shell pane.
    let mut lat = Vec::new();
    for i in 0..20 {
        lat.push(
            c.echo_rtt(probe, &format!("p{i}"), Duration::from_secs(30))
                .await,
        );
        c.pump_for(Duration::from_millis(50)).await;
    }
    let s = Summary::of(&lat);
    let (up, upd, down, downd) = proxy.stats().totals();
    let recovering = c
        .links
        .iter()
        .filter(|(_, l)| matches!(l, civet_transport::LinkState::Recovering { .. }))
        .count();
    eprintln!(
        "loss {pct}% rtt {rtt:?}: {keystrokes} keystrokes delivered in {:.0} ms, screen converged at {:.0} ms, \
         {fetched} history lines fetched; echo p50 {:.0} p95 {:.0} max {:.0} ms; \
         packets up {up} (dropped {upd}) down {down} (dropped {downd}); recovering {recovering}x, reconnects {}",
        ms(input_done),
        ms(converged),
        s.p50,
        s.p95,
        s.max,
        c.reconnect_count()
    );
    let extra = serde_json::json!({
        "loss": loss, "rtt_ms": rtt.as_millis() as u64, "keystrokes": keystrokes,
        "up": up, "up_dropped": upd, "down": down, "down_dropped": downd,
        "recovering": recovering, "reconnects": c.reconnect_count(), "fetched": fetched,
    });
    record(
        &format!("loss{pct}.input_delivered"),
        ms(input_done),
        "ms",
        extra.clone(),
    );
    record(
        &format!("loss{pct}.screen_converged"),
        ms(converged),
        "ms",
        extra,
    );
    record_summary(&format!("loss{pct}.echo"), &s, None);
    record(
        &format!("loss{pct}.correct"),
        1.0,
        "bool",
        serde_json::json!({}),
    );
    for p in [pane, probe] {
        let _ = c
            .session
            .request(civet_proto::Request::KillPane {
                pane: p,
                signal: civet_proto::KillSignal::Kill,
            })
            .await;
    }
    let _: PaneId = pane;
    c.session.close().await;
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loss_5pct_rtt_100ms() {
    loss_run(0.05).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loss_20pct_rtt_100ms() {
    loss_run(0.20).await;
}

/// Soak: 10% loss, 100 ms RTT, continuous typing for `CIVET_E2E_SOAK_SECS`
/// (default 60) while the network changes every 2 s (client rebind, NAT
/// rebinding, or a 1.5 / 4 / 7 s blackout in turn). Input must arrive exactly
/// once and in order, and the client's history must be exact.
///
/// ```sh
/// cargo test -p civet-e2e --release --test loss_latency -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: run with --release --ignored"]
async fn soak_roaming_under_loss() {
    let _serial = serial().await;
    init_logging();
    let secs: u64 = env_num("CIVET_E2E_SOAK_SECS", 60);
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(
        daemon.quic_addr(),
        Impairment::lossy(0.10, Duration::from_millis(100)),
    )
    .await
    .unwrap();
    let (_quic, dialer) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(5));
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    let out = daemon.dir.path().join("typed.txt");
    let pane = c
        .spawn(cmd_pane(
            daemon.dir.path(),
            "sh",
            &["-c", "exec cat > typed.txt"],
        ))
        .await;
    c.attach(pane).await;
    c.wait_until(WAIT, "pane", |c| c.m(pane).state > 0).await;

    let t0 = Instant::now();
    let mut next_change = t0 + Duration::from_secs(2);
    let mut changes = 0u32;
    let mut blackout_until: Option<Instant> = None;
    let mut expected = String::new();
    let mut lines = Vec::new();
    let mut i = 0usize;
    while t0.elapsed() < Duration::from_secs(secs) {
        let line = format!("soak {i:05} {}", "abcdefghij".repeat(1 + i % 4));
        for ch in line.chars().chain(std::iter::once('\r')) {
            c.session.send_input(pane, ch.to_string().into_bytes());
            expected.push(if ch == '\r' { '\n' } else { ch });
            c.pump_for(Duration::from_millis(3)).await;
        }
        lines.push(line);
        i += 1;
        let now = Instant::now();
        if blackout_until.is_some_and(|t| now >= t) {
            proxy.blackhole(false);
            c.session.notify_network_change();
            blackout_until = None;
        }
        if now >= next_change && blackout_until.is_none() {
            match changes % 3 {
                0 => c.session.notify_network_change(),
                1 => {
                    proxy.rebind_upstream().unwrap();
                }
                _ => {
                    proxy.blackhole(true);
                    // 1.5 s (stall), 4 s, 7 s (a racing re-dial starts).
                    let d = Duration::from_millis([1500, 4000, 7000][(changes as usize / 3) % 3]);
                    blackout_until = Some(now + d);
                }
            }
            changes += 1;
            next_change = now + Duration::from_secs(2);
        }
    }
    proxy.blackhole(false);
    c.session.notify_network_change();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let got = std::fs::read_to_string(&out).unwrap_or_default();
        if got.len() >= expected.len() {
            assert_eq!(
                got, expected,
                "typed text must arrive exactly once, in order"
            );
            break;
        }
        assert!(Instant::now() < deadline, "input never fully arrived");
        c.pump_for(Duration::from_millis(50)).await;
    }
    c.converge(pane, || daemon.screen(pane), Duration::from_secs(60))
        .await;
    let (history, fetched) = c.full_history(pane).await;
    let history: Vec<String> = history.into_iter().filter(|l| !l.is_empty()).collect();
    assert_eq!(history, lines);
    let recovering = c
        .links
        .iter()
        .filter(|(_, l)| matches!(l, civet_transport::LinkState::Recovering { .. }))
        .count();
    assert!(
        c.reconnects
            .iter()
            .all(|(resumed, changed)| *resumed && !changed),
        "{:?}",
        c.reconnects
    );
    eprintln!(
        "soak {secs}s: {} lines / {} keystrokes typed, {changes} network changes, \
         recovering {recovering}x, reconnects {:?}, {fetched} lines fetched: exact",
        lines.len(),
        expected.len(),
        c.reconnects
    );
    record(
        "soak.correct",
        1.0,
        "bool",
        serde_json::json!({ "secs": secs, "lines": lines.len(), "changes": changes,
            "recovering": recovering, "reconnects": c.reconnect_count() }),
    );
    c.session.close().await;
    daemon.shutdown().await;
}
