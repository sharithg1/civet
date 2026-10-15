//! Scenario 6: throughput. `cat` of a large file and `yes | head -n N` in
//! a pane, over QUIC on loopback (and the stdio tunnel for the big run).
//! Measures send-command → final screen on the client, and checks the
//! client's final screen equals the daemon's (newest-wins coalescing may
//! skip intermediate states, never the final one).
//!
//! The default run uses 5 MB / 200k lines. The plan's numbers (50 MB and
//! `yes | head -n 2000000`) are `#[ignore]`d; run them in release:
//!
//! ```sh
//! cargo test -p civet-e2e --release --test throughput -- --ignored --nocapture
//! ```

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use civet_e2e::*;
use civet_proto::{ClientId, PaneId};
use civet_transport::Dialer;
use tokio::time::Instant;

/// Write a file of numbered 64-byte lines totalling about `mb` MiB; returns
/// (path, line count, last line).
fn make_file(dir: &Path, mb: usize) -> (std::path::PathBuf, usize, String) {
    let path = dir.join(format!("big-{mb}.txt"));
    let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let lines = mb * 1024 * 1024 / 64;
    let mut last = String::new();
    for i in 0..lines {
        // 63 chars + '\n'
        last = format!(
            "line {i:08} {}",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKL:"
        );
        debug_assert_eq!(last.len(), 63);
        writeln!(f, "{last}").unwrap();
    }
    f.flush().unwrap();
    (path, lines, last)
}

/// Run `cmd; echo <marker>` and wait until the client shows the marker and
/// matches the daemon's screen. Returns (time to marker, time to converged).
async fn run_until_marker(
    c: &mut Client,
    daemon: &DaemonGuard,
    pane: PaneId,
    cmd: &str,
    marker: &str,
) -> (Duration, Duration, Duration) {
    let server_done = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let t0 = Instant::now();
    c.session
        .send_input(pane, format!("{cmd}; echo {marker}\n").into_bytes());
    // Watch the daemon's own screen to separate server time from transport.
    let watcher = {
        let sessions = daemon.d().sessions().clone();
        let marker = marker.to_string();
        let done = server_done.clone();
        tokio::spawn(async move {
            loop {
                if sessions
                    .screen_text(pane)
                    .map(|rows| rows.iter().any(|r| r.trim_end() == marker))
                    .unwrap_or(true)
                {
                    *done.lock().unwrap() = Some(Instant::now());
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };
    c.wait_until(Duration::from_secs(600), marker, |c| c.shows(pane, marker))
        .await;
    let shown = t0.elapsed();
    c.converge(pane, || daemon.screen(pane), Duration::from_secs(60))
        .await;
    let converged = t0.elapsed();
    let _ = watcher.await;
    let server = server_done.lock().unwrap().map(|t| t - t0).unwrap_or(shown);
    (shown, converged, server)
}

async fn throughput(mb: usize, yes_lines: u64, tunnel_too: bool) {
    let _serial = serial().await;
    init_logging();
    let daemon = DaemonGuard::start().await;
    let info = daemon.info();
    let (path, nlines, last) = make_file(daemon.dir.path(), mb);
    let bytes = std::fs::metadata(&path).unwrap().len();

    let mut transports: Vec<(&str, Arc<dyn Dialer>)> = vec![(
        "quic",
        quic_dialer(
            daemon.quic_addr(),
            info.cert_fingerprint,
            Duration::from_secs(5),
        )
        .1,
    )];
    if tunnel_too {
        transports.push(("tunnel", tunnel_dialer(daemon.dir.path()).0));
    }
    for (name, dialer) in transports {
        let mut c = Client::connect(dialer, &info.session_key, ClientId::generate().unwrap())
            .await
            .unwrap();
        c.transcripts = false;
        let pane = c.spawn(sh_pane(daemon.dir.path())).await;
        c.attach(pane).await;
        c.echo_rtt(pane, "ready", WAIT).await;

        // cat
        let (shown, converged, server) = run_until_marker(
            &mut c,
            &daemon,
            pane,
            &format!("cat {}", path.display()),
            "CAT-DONE",
        )
        .await;
        let rows = trim_rows(&c.m(pane).rows());
        let at = rows
            .iter()
            .position(|r| r == "CAT-DONE")
            .expect("marker row");
        assert_eq!(
            rows[at - 1],
            last,
            "the file's last line is right above the marker"
        );
        let (updates, recv_bytes) = (c.m(pane).fulls + c.m(pane).diffs, c.m(pane).bytes);
        let mbps = bytes as f64 / 1_048_576.0 / shown.as_secs_f64();
        eprintln!(
            "[{name}] cat {:.1} MiB ({nlines} lines): client shows final screen after {:.0} ms \
             ({mbps:.1} MiB/s), converged {:.0} ms, daemon itself done at {:.0} ms; \
             {updates} updates, {:.2} MiB of VT received",
            bytes as f64 / 1_048_576.0,
            ms(shown),
            ms(converged),
            ms(server),
            recv_bytes as f64 / 1_048_576.0
        );
        let extra = serde_json::json!({
            "transport": name, "bytes": bytes, "lines": nlines, "daemon_done_ms": ms(server),
            "converged_ms": ms(converged), "updates": updates, "vt_bytes": recv_bytes,
        });
        record(
            &format!("throughput.cat{mb}mb.{name}"),
            ms(shown),
            "ms",
            extra.clone(),
        );
        record(
            &format!("throughput.cat{mb}mb.{name}.mibps"),
            mbps,
            "MiB/s",
            extra,
        );

        // yes | head
        let before = (c.m(pane).fulls + c.m(pane).diffs, c.m(pane).bytes);
        let (shown, converged, server) = run_until_marker(
            &mut c,
            &daemon,
            pane,
            &format!("yes | head -n {yes_lines}"),
            "YES-DONE",
        )
        .await;
        let rows = trim_rows(&c.m(pane).rows());
        let at = rows
            .iter()
            .position(|r| r == "YES-DONE")
            .expect("marker row");
        assert_eq!(rows[at - 1], "y");
        let updates = c.m(pane).fulls + c.m(pane).diffs - before.0;
        eprintln!(
            "[{name}] yes | head -n {yes_lines}: final screen after {:.0} ms, converged {:.0} ms, \
             daemon done at {:.0} ms; {updates} updates, {:.2} MiB of VT",
            ms(shown),
            ms(converged),
            ms(server),
            (c.m(pane).bytes - before.1) as f64 / 1_048_576.0
        );
        record(
            &format!("throughput.yes{yes_lines}.{name}"),
            ms(shown),
            "ms",
            serde_json::json!({ "daemon_done_ms": ms(server), "converged_ms": ms(converged), "updates": updates }),
        );
        // Still interactive afterwards.
        let echo = c.echo_rtt(pane, "after", WAIT).await;
        record(
            &format!("throughput.echo_after.{name}"),
            ms(echo),
            "ms",
            serde_json::json!({}),
        );
        c.session.close().await;
    }
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throughput_small() {
    throughput(5, 200_000, false).await;
}

/// The plan's throughput test: 50 MB `cat`, `yes | head -n 2000000`, over
/// QUIC and the stdio tunnel. Run in release.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: run with --release --ignored"]
async fn throughput_50mb() {
    throughput(50, 2_000_000, true).await;
}
