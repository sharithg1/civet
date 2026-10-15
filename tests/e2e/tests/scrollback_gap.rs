//! Scenario 7: scrollback fetch after a resume gap. While the client is
//! away, a pane prints far more than one Diff replays (`MAX_REPLAY_LINES` =
//! 2000). When it is back the client gets a Diff whose `history_start` is
//! past what it has (a HISTORY_GAP), fills the gap with
//! `ScrollbackQuery::Range` pages, and pages the whole history upward with
//! `ScrollbackQuery::Before`: both must give the exact, contiguous output.
//!
//! * `scrollback_fetch_after_resume_gap`: the connection is lost (link cut,
//!   dials refused for 2 s) and the session resumes over a new one, so the
//!   daemon diffs from the client's acked state: the gap is deterministic.
//! * `scrollback_after_blackhole`: QUIC blackhole instead. Updates sent
//!   during the blackout are retransmitted by QUIC (each replaying up to
//!   2000 lines), so whether a gap appears depends on timing; the history
//!   must be complete either way.

use std::time::Duration;

use civet_e2e::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use civet_proto::{ClientId, MAX_REPLAY_LINES, PaneId, ScrollbackQuery};
use civet_transport::{Dialer, StreamDialer};
use tokio::time::Instant;

const LINES: u64 = 6000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrollback_after_blackhole() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let proxy = UdpProxy::start(
        daemon.quic_addr(),
        Impairment::rtt(Duration::from_millis(20)),
    )
    .await
    .unwrap();
    let (_q, dialer) = quic_dialer(proxy.addr(), info.cert_fingerprint, Duration::from_secs(5));
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    let pane = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(pane).await;
    c.echo_rtt(pane, "ready", WAIT).await;
    c.session.send_input(
        pane,
        format!("sleep 1; seq 1 {LINES} | sed 's/^/g /'; echo GAP-DONE\n").into_bytes(),
    );
    c.wait_until(WAIT, "command echoed", |c| {
        c.m(pane).rows().iter().any(|r| r.contains("echo GAP-DONE"))
    })
    .await;
    proxy.blackhole(true);
    let deadline = Instant::now() + WAIT;
    while !daemon.screen(pane).iter().any(|r| r == "GAP-DONE") {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    proxy.blackhole(false);
    c.session.notify_network_change();
    c.wait_until(WAIT, "GAP-DONE", |c| c.shows(pane, "GAP-DONE"))
        .await;
    c.converge(pane, || daemon.screen(pane), WAIT).await;

    check_history(&mut c, pane, "blackhole").await;
    c.session.close().await;
    daemon.shutdown().await;
}

/// Unix-socket dialer whose link can be cut and whose dials can be refused.
fn gated_dialer(
    socket: std::path::PathBuf,
) -> (
    Arc<dyn Dialer>,
    tokio::sync::watch::Sender<u32>,
    Arc<AtomicBool>,
) {
    let (cut_tx, cut_rx) = tokio::sync::watch::channel(0u32);
    let refuse = Arc::new(AtomicBool::new(false));
    let refuse2 = refuse.clone();
    let dialer: Arc<dyn Dialer> = Arc::new(StreamDialer {
        factory: move || {
            let socket = socket.clone();
            let mut cut = cut_rx.clone();
            let refuse = refuse2.clone();
            async move {
                if refuse.load(Ordering::SeqCst) {
                    return Err(std::io::Error::other("network unreachable"));
                }
                let s = tokio::net::UnixStream::connect(&socket).await?;
                let (ours, theirs) = tokio::io::duplex(1 << 16);
                tokio::spawn(async move {
                    let (mut a, mut b) = (s, theirs);
                    cut.borrow_and_update();
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                        _ = cut.changed() => {}
                    }
                });
                Ok(ours)
            }
        },
    });
    (dialer, cut_tx, refuse)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrollback_fetch_after_resume_gap() {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let (dialer, cut, refuse) = gated_dialer(daemon.d().socket_path());
    let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
        .await
        .unwrap();
    let pane = c.spawn(sh_pane(daemon.dir.path())).await;
    c.attach(pane).await;
    c.echo_rtt(pane, "ready", WAIT).await;
    c.session.send_input(
        pane,
        format!("sleep 1; seq 1 {LINES} | sed 's/^/g /'; echo GAP-DONE\n").into_bytes(),
    );
    c.wait_until(WAIT, "command echoed", |c| {
        c.m(pane).rows().iter().any(|r| r.contains("echo GAP-DONE"))
    })
    .await;
    // The connection is lost and cannot be re-established for a while.
    refuse.store(true, Ordering::SeqCst);
    cut.send(1).unwrap();
    let deadline = Instant::now() + WAIT;
    while !daemon.screen(pane).iter().any(|r| r == "GAP-DONE") {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    refuse.store(false, Ordering::SeqCst);
    c.session.notify_network_change();
    c.wait_until(WAIT, "GAP-DONE", |c| c.shows(pane, "GAP-DONE"))
        .await;
    assert!(
        c.reconnects.iter().any(|(resumed, _)| *resumed),
        "must resume: {:?}",
        c.reconnects
    );
    c.converge(pane, || daemon.screen(pane), WAIT).await;
    let gaps = c.m(pane).gaps.clone();
    let gap_lines: u64 = gaps.iter().map(|(s, e)| e - s).sum();
    assert!(
        gap_lines + MAX_REPLAY_LINES + 30 >= LINES,
        "a resume replays at most {MAX_REPLAY_LINES} lines: gaps {gaps:?}; updates {:?}",
        c.m(pane).headers
    );
    check_history(&mut c, pane, "resume").await;
    c.session.close().await;
    daemon.shutdown().await;
}

async fn check_history(c: &mut Client, pane: PaneId, label: &str) {
    let gaps = c.m(pane).gaps.clone();
    let gap_lines: u64 = gaps.iter().map(|(s, e)| e - s).sum();

    // Fill the gap with Range pages (1000 lines each).
    let t0 = Instant::now();
    let (history, fetched) = c.full_history(pane).await;
    let fill = t0.elapsed();
    let expected: Vec<String> = (1..=LINES).map(|i| format!("g {i}")).collect();
    assert_contiguous(&history, &expected);

    // Page the whole history upward with Before (what scrolling up does).
    let t1 = Instant::now();
    let end = c.m(pane).history_lines;
    let mut cursor = end;
    let mut pages = 0;
    let mut upward: Vec<String> = Vec::new();
    loop {
        let page = c
            .session
            .fetch_scrollback(
                pane,
                ScrollbackQuery::Before {
                    end: Some(cursor),
                    limit: 500,
                },
            )
            .await
            .unwrap();
        pages += 1;
        if page.lines.is_empty() {
            break;
        }
        let mut lines: Vec<String> = page.lines.iter().map(|l| strip_vt(&l.vt)).collect();
        lines.append(&mut upward);
        upward = lines;
        cursor = page.start;
        if cursor <= page.oldest_available {
            break;
        }
    }
    let paged = t1.elapsed();
    upward.extend(trim_rows(&c.m(pane).rows()));
    assert_contiguous(&upward, &expected);

    eprintln!(
        "[{label}] gaps {gaps:?} ({gap_lines} lines); filled {fetched} lines in {:.1} ms; \
         paged {end} lines upward in {pages} Before-pages in {:.1} ms",
        ms(fill),
        ms(paged)
    );
    record(
        &format!("scrollback_gap.{label}.fill"),
        ms(fill),
        "ms",
        serde_json::json!({ "lines": fetched }),
    );
    record(
        &format!("scrollback_gap.{label}.page_all_before"),
        ms(paged),
        "ms",
        serde_json::json!({ "lines": end, "pages": pages }),
    );
}
