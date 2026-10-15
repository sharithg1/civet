//! Daemon regression tests for review findings: oversized pane sizes are
//! capped (no OOM abort), and a pane whose screen + history replay would
//! exceed the frame limit keeps updating (the screen task used to die).

#![cfg(feature = "server")]

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use civet_proto::{
    ClientId, CommandSpec, MAX_TERM_COLS, MAX_TERM_ROWS, Request, Response, ScrollbackQuery,
    SpawnPaneRequest, TermSize,
};
use civet_term::{PaneTerminal, TermConfig};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, QuicClient, QuicClientConfig, QuicDialer,
    SessionEvent,
};
use civetd::daemon::{Daemon, DaemonOptions};

const WAIT: Duration = Duration::from_secs(60);

async fn start() -> (
    Daemon,
    tempfile::TempDir,
    ClientSession,
    tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut o = DaemonOptions::new(dir.path());
    o.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    o.port_range = 0..=0;
    o.shell = Some("/bin/sh".into());
    let daemon = Daemon::start(o).await.unwrap();
    let info = daemon.info().clone();
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client: Arc::new(QuicClient::new(false).unwrap()),
        config: QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint),
    });
    let cfg = ClientSessionConfig::new(
        info.session_key.clone(),
        ClientId::generate().unwrap(),
        "hardening",
        "0",
    );
    let (s, ev) = ClientSession::connect(cfg, dialer).await.unwrap();
    (daemon, dir, s, ev)
}

fn spawn_req(cwd: &Path, size: TermSize, program: &str, args: &[&str]) -> Request {
    Request::SpawnPane(SpawnPaneRequest {
        worktree: None,
        cwd: Some(cwd.display().to_string()),
        command: Some(CommandSpec::new(
            program,
            args.iter().map(|s| s.to_string()),
        )),
        env: vec![("PS1".into(), "$ ".into())],
        size,
        title: None,
        agent: None,
    })
}

/// 65535x65535 from SpawnPane / ResizePane used to allocate every cell and
/// abort the daemon (killing every pane). Now the size is capped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_pane_sizes_are_capped() {
    let (daemon, dir, s, _ev) = start().await;
    let huge = TermSize::new(u16::MAX, u16::MAX);
    let info = match s
        .request(spawn_req(dir.path(), huge, "sh", &[]))
        .await
        .unwrap()
    {
        Response::PaneSpawned(p) => p,
        other => panic!("{other:?}"),
    };
    assert!(
        info.size.cols <= MAX_TERM_COLS && info.size.rows <= MAX_TERM_ROWS,
        "{:?}",
        info.size
    );
    assert!(matches!(
        s.request(Request::ResizePane {
            pane: info.id,
            size: huge
        })
        .await
        .unwrap(),
        Response::Ok
    ));
    match s.request(Request::ListPanes).await.unwrap() {
        Response::Panes(p) => {
            let size = p.iter().find(|p| p.id == info.id).unwrap().size;
            assert_eq!((size.cols, size.rows), (MAX_TERM_COLS, MAX_TERM_ROWS));
        }
        other => panic!("{other:?}"),
    }
    // The daemon is alive and serving.
    assert!(matches!(
        s.request(Request::Ping { nonce: 1 }).await.unwrap(),
        Response::Pong { .. }
    ));
    s.close().await;
    daemon.shutdown("test done").await;
}

/// History whose replay is far beyond the 16 MiB frame limit (wide lines,
/// a truecolor change on every cell). A subscriber's first update (Full
/// with replay) used to fail to encode, which ended the screen task: the
/// pane froze for that client. Now the update fits and the pane keeps
/// updating.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn huge_replay_does_not_freeze_the_pane() {
    let (daemon, dir, s, mut ev) = start().await;
    let script = r#"awk 'BEGIN { for (i = 0; i < 1200; i++) { s = ""; for (j = 0; j < 1000; j++) s = s sprintf("\033[38;2;%d;%d;%dm#", i % 256, j % 256, (i + j) % 256); print s } }'; printf '\033[0m'; echo HEAVY-DONE; exec sh"#;
    let pane = match s
        .request(spawn_req(
            dir.path(),
            TermSize::new(1000, 50),
            "sh",
            &["-c", script],
        ))
        .await
        .unwrap()
    {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    // Wait until the history is there (before subscribing, so the first
    // update must replay it).
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let page = s
            .fetch_scrollback(
                pane,
                ScrollbackQuery::Before {
                    end: None,
                    limit: 1,
                },
            )
            .await
            .unwrap();
        if page.end_available >= 1150 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "awk too slow: {}",
            page.end_available
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    s.subscribe(pane).await.unwrap();
    let mut mirror = PaneTerminal::new(TermConfig {
        size: TermSize::new(1000, 50),
        ..TermConfig::default()
    });
    let shows = |m: &PaneTerminal, w: &str| m.screen_text().iter().any(|r| r.trim_end() == w);
    let mut first = None;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !shows(&mirror, "HEAVY-DONE") {
        match tokio::time::timeout_at(deadline, ev.recv()).await {
            Ok(Some(SessionEvent::Screen(u))) if u.pane == pane => {
                assert!(u.payload.len() < civet_proto::MAX_FRAME_LEN);
                first.get_or_insert((u.history_start, u.history_lines));
                if u.size != mirror.size() {
                    mirror.resize(u.size).unwrap();
                }
                mirror.feed(&u.payload);
            }
            Ok(Some(_)) => {}
            other => panic!("pane froze: {other:?} first={first:?}"),
        }
    }
    let (start, lines) = first.unwrap();
    assert!(start <= lines);
    // And it keeps updating.
    s.send_input(pane, b"echo STILL-LIVE\n".to_vec());
    while !shows(&mirror, "STILL-LIVE") {
        match tokio::time::timeout_at(deadline, ev.recv()).await {
            Ok(Some(SessionEvent::Screen(u))) if u.pane == pane => {
                mirror.feed(&u.payload);
            }
            Ok(Some(_)) => {}
            other => panic!("pane froze after the replay: {other:?}"),
        }
    }
    s.close().await;
    daemon.shutdown("test done").await;
}
