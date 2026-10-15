//! Scenario 8: idle daemon memory. The real `civetd serve` binary (same
//! profile as the test: run with `--release` for real numbers) with one
//! QUIC client attached; RSS is sampled with `ps -o rss=`.
//!
//! * baseline: daemon + connected client, no panes;
//! * 10 idle shell panes (`/bin/sh`), all subscribed: RSS delta / 10 is the
//!   idle per-session cost (plan target < 20 MB);
//! * then every pane's scrollback is filled to its 10 000-line limit with
//!   ~70-column lines (worst case for an idle pane that was busy before).
//!
//! Also reports the shells' own RSS (not part of the daemon's).

use std::time::Duration;

use civet_e2e::*;
use civet_proto::{ClientId, PaneId};

const PANES: usize = 10;

async fn settle_rss(pid: u32) -> u64 {
    // Let allocations settle, then take the max of a few samples.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut v = 0;
    for _ in 0..5 {
        v = v.max(rss_kib(pid).unwrap_or(0));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_daemon_memory_per_session() {
    let _serial = serial().await;
    init_logging();
    let dir = tempfile::tempdir().unwrap();
    let daemon = ChildDaemon::start(dir.path(), free_udp_port())
        .await
        .unwrap();
    let pid = daemon.pid();
    let (_q, dialer) = quic_dialer(
        daemon.addr(),
        daemon.info.cert_fingerprint,
        Duration::from_secs(5),
    );
    let mut c = Client::connect(
        dialer,
        &daemon.info.session_key,
        ClientId::generate().unwrap(),
    )
    .await
    .unwrap();
    c.transcripts = false;
    // Warm up code paths once (TLS, first pane) so the baseline is fair.
    let warm = c.spawn(sh_pane(dir.path())).await;
    c.attach(warm).await;
    c.echo_rtt(warm, "warm", WAIT).await;
    c.session
        .request(civet_proto::Request::KillPane {
            pane: warm,
            signal: civet_proto::KillSignal::Kill,
        })
        .await
        .unwrap();
    c.mirrors.clear();
    let base = settle_rss(pid).await;

    let mut panes: Vec<PaneId> = Vec::new();
    for _ in 0..PANES {
        let p = c.spawn(sh_pane(dir.path())).await;
        c.attach(p).await;
        panes.push(p);
    }
    for (i, p) in panes.iter().enumerate() {
        c.echo_rtt(*p, &format!("idle{i}"), WAIT).await;
    }
    c.pump_for(Duration::from_millis(500)).await;
    let idle = settle_rss(pid).await;
    let shells_kib: u64 = child_pids(pid).iter().filter_map(|p| rss_kib(*p)).sum();

    // Fill each pane's scrollback to the 10 000-line limit.
    for (i, p) in panes.iter().enumerate() {
        c.session.send_input(
            *p,
            format!(
                "yes 'scrollback filler line for the idle memory test, 70 columns wide..' | head -n 10100 | cat -n; echo FILLED{i}\n"
            )
            .into_bytes(),
        );
    }
    for (i, p) in panes.iter().enumerate() {
        let marker = format!("FILLED{i}");
        c.wait_until(Duration::from_secs(120), &marker, |c| c.shows(*p, &marker))
            .await;
    }
    c.pump_for(Duration::from_millis(500)).await;
    let full = settle_rss(pid).await;

    let per_idle_mib = (idle.saturating_sub(base)) as f64 / PANES as f64 / 1024.0;
    let per_full_mib = (full.saturating_sub(base)) as f64 / PANES as f64 / 1024.0;
    eprintln!(
        "daemon RSS: baseline {:.1} MiB; {PANES} idle shells {:.1} MiB (+{per_idle_mib:.2} MiB/session); \
         after filling 10k-line scrollback each {:.1} MiB (+{per_full_mib:.2} MiB/session); \
         the {PANES} shells themselves {:.1} MiB",
        base as f64 / 1024.0,
        idle as f64 / 1024.0,
        full as f64 / 1024.0,
        shells_kib as f64 / 1024.0
    );
    let extra = serde_json::json!({
        "baseline_kib": base, "idle_kib": idle, "full_kib": full, "panes": PANES,
        "shells_kib": shells_kib, "target_mib": 20,
    });
    record(
        "memory.daemon_baseline",
        base as f64 / 1024.0,
        "MiB",
        extra.clone(),
    );
    record(
        "memory.per_idle_session",
        per_idle_mib,
        "MiB",
        extra.clone(),
    );
    record(
        "memory.per_session_full_scrollback",
        per_full_mib,
        "MiB",
        extra,
    );
    assert!(
        per_idle_mib < 20.0,
        "idle per-session memory {per_idle_mib:.2} MiB >= 20 MiB"
    );
    c.session.close().await;
    daemon.terminate().await;
}
