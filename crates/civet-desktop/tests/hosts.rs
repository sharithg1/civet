//! Drives the command layer (`HostManager`, which every Tauri command wraps)
//! against a real in-process `civetd` daemon (QUIC and the unix-socket stream
//! link), with recording UI and channel sinks.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use civet_desktop::batch::RecordSink;
use civet_desktop::connect::{dial, session_config};
use civet_desktop::error::CmdError;
use civet_desktop::events::{
    AgentAttentionEvent, BootstrapProgressEvent, ClipboardWrite, DaemonEventPayload, HostState,
    HostStateEvent, HostTarget, TransportChoice, UiEvents,
};
use civet_desktop::hosts::{Connector, Established, HostManager, ManagerConfig, Progress};
use civet_desktop::records::{RecordHeader, RecordKind, decode_all, flags};
use civet_proto::{
    AgentStatus, BootstrapInfo, ClientId, CommandSpec, KillSignal, PaneId, ScrollbackQuery,
    SpawnPaneRequest, TermSize,
};
use civet_term::{PaneTerminal, TermConfig};
use civet_transport::{StreamDialer, TransportKind};
use civetd::daemon::{Daemon, DaemonOptions};
use futures::future::BoxFuture;
use tokio::sync::mpsc;

const T: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RecordingUi {
    states: Mutex<Vec<HostStateEvent>>,
    progress: Mutex<Vec<BootstrapProgressEvent>>,
    events: Mutex<Vec<DaemonEventPayload>>,
    attention: Mutex<Vec<AgentAttentionEvent>>,
    notes: Mutex<Vec<(String, String)>>,
    clipboard: Mutex<Vec<ClipboardWrite>>,
    focused: Mutex<bool>,
}

impl RecordingUi {
    fn states(&self) -> Vec<HostState> {
        self.states
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.state)
            .collect()
    }
}

impl UiEvents for RecordingUi {
    fn host_state(&self, e: HostStateEvent) {
        self.states.lock().unwrap().push(e);
    }
    fn bootstrap_progress(&self, e: BootstrapProgressEvent) {
        self.progress.lock().unwrap().push(e);
    }
    fn daemon_event(&self, e: DaemonEventPayload) {
        self.events.lock().unwrap().push(e);
    }
    fn agent_attention(&self, e: AgentAttentionEvent) {
        self.attention.lock().unwrap().push(e);
    }
    fn window_focused(&self) -> bool {
        *self.focused.lock().unwrap()
    }
    fn clipboard(&self, write: ClipboardWrite) {
        self.clipboard.lock().unwrap().push(write);
    }
    fn notify(&self, title: &str, body: &str) {
        self.notes
            .lock()
            .unwrap()
            .push((title.to_string(), body.to_string()));
    }
}

/// "Bootstraps" by reading `daemon.json` from a state dir (waiting for one
/// with a session key not used before, so a restarted daemon is picked up),
/// then dials with the app's own `dial` (QUIC, or the unix socket as the
/// stream link for `TransportChoice::Ssh`).
struct DirConnector {
    state_dir: PathBuf,
    client_id: ClientId,
    used_keys: Mutex<Vec<String>>,
}

impl DirConnector {
    async fn wait_info(&self) -> Result<BootstrapInfo, CmdError> {
        let file = self.state_dir.join("daemon.json");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(s) = std::fs::read_to_string(&file)
                && let Ok(info) = BootstrapInfo::from_json_line(&s)
            {
                let key = info.session_key.to_hex();
                let mut used = self.used_keys.lock().unwrap();
                if !used.contains(&key) {
                    used.push(key);
                    return Ok(info);
                }
            }
            if tokio::time::Instant::now() > deadline {
                return Err(CmdError::bootstrap("no daemon"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Connector for DirConnector {
    fn connect<'a>(
        &'a self,
        _target: &'a HostTarget,
        choice: TransportChoice,
        _replace: bool,
        progress: Progress,
    ) -> BoxFuture<'a, Result<Established, CmdError>> {
        Box::pin(async move {
            progress(civet_desktop::events::ProgressStep::Start, None);
            let info = self.wait_info().await?;
            let mut cfg = session_config(info.session_key.clone(), self.client_id);
            cfg.heartbeat = Duration::from_millis(200);
            cfg.stall_after = Duration::from_millis(300);
            cfg.race_after = Duration::from_millis(600);
            let sock = self.state_dir.join("civetd.sock");
            let tunnel = Arc::new(StreamDialer {
                factory: move || tokio::net::UnixStream::connect(sock.clone()),
            });
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], info.udp_port));
            dial(
                cfg,
                choice,
                Some((addr, info.cert_fingerprint)),
                tunnel,
                info.daemon_version.clone(),
                None,
                &progress,
            )
            .await
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    state_dir: PathBuf,
    daemon: Option<Daemon>,
    ui: Arc<RecordingUi>,
    mgr: HostManager,
}

fn daemon_opts(dir: &Path) -> DaemonOptions {
    let mut o = DaemonOptions::new(dir);
    o.bind_ip = std::net::IpAddr::from([127, 0, 0, 1]);
    o.port_range = 0..=0;
    o.shell = Some("/bin/sh".into());
    o
}

impl Rig {
    async fn new(cfg: ManagerConfig) -> Rig {
        let dir = tempfile::Builder::new()
            .prefix("cvt")
            .tempdir_in("/tmp")
            .unwrap();
        let state_dir = dir.path().to_path_buf();
        let daemon = Daemon::start(daemon_opts(&state_dir)).await.unwrap();
        let ui = Arc::new(RecordingUi::default());
        let connector = Arc::new(DirConnector {
            state_dir: state_dir.clone(),
            client_id: ClientId::generate().unwrap(),
            used_keys: Mutex::new(Vec::new()),
        });
        let mgr = HostManager::new(
            cfg,
            ui.clone(),
            connector,
            tokio::runtime::Handle::current(),
        );
        Rig {
            _dir: dir,
            state_dir,
            daemon: Some(daemon),
            ui,
            mgr,
        }
    }

    async fn restart_daemon(&mut self, pause: Duration) {
        if let Some(d) = self.daemon.take() {
            d.shutdown("test restart").await;
        }
        tokio::time::sleep(pause).await;
        self.daemon = Some(Daemon::start(daemon_opts(&self.state_dir)).await.unwrap());
    }
}

fn fast() -> ManagerConfig {
    ManagerConfig {
        detach_linger: Duration::from_millis(300),
        ..ManagerConfig::default()
    }
}

/// A channel sink plus a terminal that plays every record like xterm.js.
struct Screen {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    term: PaneTerminal,
    records: Vec<(RecordHeader, Vec<u8>)>,
}

impl Screen {
    fn new() -> (Arc<dyn RecordSink>, Screen) {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        (
            Arc::new(tx),
            Screen {
                rx,
                term: PaneTerminal::new(TermConfig {
                    size: TermSize::new(80, 24),
                    scrollback_limit: 2000,
                }),
                records: Vec::new(),
            },
        )
    }

    fn apply(&mut self, msg: &[u8]) {
        for (h, p) in decode_all(msg).unwrap() {
            if matches!(h.kind, RecordKind::Screen | RecordKind::Predict)
                && h.cols > 0
                && (h.cols, h.rows) != (self.term.size().cols, self.term.size().rows)
            {
                self.term.resize(TermSize::new(h.cols, h.rows)).unwrap();
            }
            self.term.feed(p);
            self.records.push((h, p.to_vec()));
        }
    }

    /// Apply records until `pred` holds.
    async fn until(&mut self, what: &str, mut pred: impl FnMut(&Screen) -> bool) {
        let fut = async {
            while !pred(self) {
                let msg = self.rx.recv().await.expect("channel closed");
                self.apply(&msg);
            }
        };
        if tokio::time::timeout(T, fut).await.is_err() {
            panic!(
                "timed out waiting for {what}; screen: {:?}; kinds: {:?}",
                self.term.screen_text(),
                self.records.iter().map(|r| r.0.kind).collect::<Vec<_>>()
            );
        }
    }

    fn text(&self) -> String {
        self.term.screen_text().join("\n")
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn shell_pane(script: &str) -> SpawnPaneRequest {
    SpawnPaneRequest {
        worktree: None,
        cwd: Some("/tmp".into()),
        command: Some(CommandSpec::new("/bin/sh", ["-c", script])),
        env: Vec::new(),
        size: TermSize::new(80, 24),
        title: Some("test pane".into()),
        agent: None,
    }
}

fn agent_pane(script: &str) -> SpawnPaneRequest {
    SpawnPaneRequest {
        agent: Some("test-agent".into()),
        ..shell_pane(script)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_over_quic_against_real_daemon() {
    let rig = Rig::new(fast()).await;
    let m = &rig.mgr;
    let h = "local";

    let summary = m
        .connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    assert_eq!(summary.state, HostState::Connected);
    assert_eq!(summary.transport, Some(TransportKind::Quic));
    assert!(summary.instance_id.is_some());
    assert_eq!(m.list_hosts().len(), 1);
    // Idempotent while connected.
    assert_eq!(
        m.connect_host(HostTarget::Local, TransportChoice::Auto)
            .await
            .unwrap()
            .instance_id,
        summary.instance_id
    );
    assert!(rig.ui.states().contains(&HostState::Connected));
    assert!(m.ping(h).await.unwrap().rtt_ms < 1000);
    assert_eq!(m.daemon_info(h).await.unwrap().proto_version, 1);
    assert!(m.list_repos(h).await.unwrap().is_empty());
    assert_eq!(m.list_repos("nope").await.unwrap_err().code, "noSuchHost");

    // Spawn + attach: the first record is a Full; output and echo arrive.
    let pane = m
        .spawn_pane(h, shell_pane("printf 'hello from civet\\n'; exec cat"))
        .await
        .unwrap();
    let (sink, mut screen) = Screen::new();
    let info = m.attach_pane(h, pane.id, sink).await.unwrap();
    assert_eq!(info.pane, pane.id);
    screen
        .until("first record", |s| !s.records.is_empty())
        .await;
    assert_eq!(screen.records[0].0.kind, RecordKind::Screen);
    assert_ne!(
        screen.records[0].0.flags & flags::FULL,
        0,
        "first record is a Full"
    );
    screen
        .until("hello", |s| s.text().contains("hello from civet"))
        .await;

    let seq1 = m.write_input(h, pane.id, b"typed text\r".to_vec()).unwrap();
    let seq2 = m.write_input(h, pane.id, b"more\r".to_vec()).unwrap();
    assert_eq!((seq1, seq2), (1, 2));
    screen
        .until("echo", |s| s.text().matches("more").count() >= 2)
        .await;
    screen
        .until("input ack", |s| {
            s.records.iter().any(|(h, _)| h.acked_input_seq >= 2)
        })
        .await;

    // Resize: a record with the new size; resizing to the same size again
    // still yields a record carrying it.
    m.resize_pane(h, pane.id, TermSize::new(100, 30))
        .await
        .unwrap();
    screen
        .until("resize", |s| {
            s.records.iter().any(|(h, _)| (h.cols, h.rows) == (100, 30))
        })
        .await;
    let n = screen.records.len();
    m.resize_pane(h, pane.id, TermSize::new(100, 30))
        .await
        .unwrap();
    screen
        .until("same-size resize record", |s| s.records.len() > n)
        .await;
    assert_eq!(
        (screen.records[n].0.cols, screen.records[n].0.rows),
        (100, 30)
    );

    // Scrollback.
    let page = m
        .fetch_scrollback(
            h,
            pane.id,
            ScrollbackQuery::Before {
                end: None,
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.lines.len(), page.wrapped.len());

    // Detach + re-attach (UI reload): instant Full from the client cache.
    m.detach_pane(h, pane.id).unwrap();
    let (sink2, mut screen2) = Screen::new();
    let t0 = std::time::Instant::now();
    let info2 = m.attach_pane(h, pane.id, sink2).await.unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(50),
        "cached attach is instant"
    );
    assert_eq!(info2.size, TermSize::new(100, 30));
    screen2
        .until("cached full", |s| !s.records.is_empty())
        .await;
    assert_ne!(screen2.records[0].0.flags & flags::FULL, 0);
    assert_eq!(screen2.term.screen_text(), screen.term.screen_text());
    m.write_input(h, pane.id, b"after reattach\r".to_vec())
        .unwrap();
    screen2
        .until("live after reattach", |s| {
            s.text().contains("after reattach")
        })
        .await;

    // A plain shell pane never asks for attention (typing `exit` in a
    // shell must not notify).
    m.set_agent_status(h, pane.id, AgentStatus::Done)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rig.ui.attention.lock().unwrap().is_empty(), "shell pane");
    assert!(rig.ui.notes.lock().unwrap().is_empty(), "shell pane");
    m.kill_pane(h, pane.id, Some(KillSignal::Kill))
        .await
        .unwrap();

    // An agent pane: attention event + native notification (unfocused),
    // rate-limited per (pane, status).
    let pane = m.spawn_pane(h, agent_pane("exec cat")).await.unwrap();
    let (sink2, mut screen2) = Screen::new();
    m.attach_pane(h, pane.id, sink2).await.unwrap();
    screen2.until("agent full", |s| !s.records.is_empty()).await;
    m.set_agent_status(h, pane.id, AgentStatus::Running)
        .await
        .unwrap();
    m.set_agent_status(h, pane.id, AgentStatus::Done)
        .await
        .unwrap();
    eventually("attention", || !rig.ui.attention.lock().unwrap().is_empty()).await;
    {
        let a = rig.ui.attention.lock().unwrap()[0].clone();
        assert_eq!(a.status, AgentStatus::Done);
        assert_eq!(a.title, "test pane");
    }
    eventually("notification", || !rig.ui.notes.lock().unwrap().is_empty()).await;
    assert_eq!(rig.ui.notes.lock().unwrap()[0].1, "Agent finished");
    m.set_agent_status(h, pane.id, AgentStatus::NeedsInput)
        .await
        .unwrap();
    eventually("second attention", || {
        rig.ui.attention.lock().unwrap().len() == 2
    })
    .await;
    // A different status of the same pane is news: not throttled.
    eventually("needs-input notification", || {
        rig.ui.notes.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(rig.ui.notes.lock().unwrap()[1].1, "Agent needs input");
    // Done again within the interval: the event, but no second notification.
    m.set_agent_status(h, pane.id, AgentStatus::Done)
        .await
        .unwrap();
    eventually("third attention", || {
        rig.ui.attention.lock().unwrap().len() == 3
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.ui.notes.lock().unwrap().len(), 2, "rate limited");

    // Daemon events are forwarded.
    assert!(
        rig.ui
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e.event, civet_proto::Event::PaneCreated(_)))
    );

    // Network change hint: rebind + ping; the pane stays live.
    m.notify_network_change();
    m.write_input(h, pane.id, b"after rebind\r".to_vec())
        .unwrap();
    screen2
        .until("live after rebind", |s| s.text().contains("after rebind"))
        .await;

    // Kill: kind 3 on the channel; a second kill is notFound.
    m.kill_pane(h, pane.id, Some(KillSignal::Kill))
        .await
        .unwrap();
    screen2
        .until("pane closed", |s| {
            s.records
                .iter()
                .any(|(h, _)| h.kind == RecordKind::PaneClosed)
        })
        .await;
    let err = m.kill_pane(h, pane.id, None).await.unwrap_err();
    assert_eq!(err.code, "notFound");
    let err = m
        .attach_pane(h, PaneId(9999), Screen::new().0)
        .await
        .unwrap_err();
    assert_eq!(err.code, "notFound");

    m.disconnect_host(h).await.unwrap();
    assert!(m.list_hosts().is_empty());
    assert_eq!(*rig.ui.states().last().unwrap(), HostState::Disconnected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_link_repos_worktrees_and_agents() {
    let rig = Rig::new(fast()).await;
    let m = &rig.mgr;
    let h = "local";
    let s = m
        .connect_host(HostTarget::Local, TransportChoice::Ssh)
        .await
        .unwrap();
    assert_eq!(s.transport, Some(TransportKind::Stream));

    // A git repo with one commit.
    let repo_dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_dir.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}: {st:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);

    let repo = m
        .add_repo(h, repo_dir.path().display().to_string())
        .await
        .unwrap();
    assert_eq!(m.list_repos(h).await.unwrap().len(), 1);
    let err = m
        .add_repo(h, repo_dir.path().display().to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "alreadyExists");
    let wt = m
        .create_worktree(h, repo.id.clone(), "feature-x".into(), None, None)
        .await
        .unwrap();
    assert_eq!(wt.branch.as_deref(), Some("feature-x"));
    assert_eq!(m.list_worktrees(h, repo.id.clone()).await.unwrap().len(), 2);

    let launched = m
        .launch_agent(
            h,
            civet_proto::LaunchAgentRequest {
                repo: repo.id.clone(),
                branch: "agent-1".into(),
                base: None,
                worktree_path: None,
                command: CommandSpec::new("/bin/sh", ["-c", "echo agent running; sleep 30"]),
                env: Vec::new(),
                size: TermSize::new(80, 24),
                title: None,
                agent: Some("test-agent".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(launched.worktree.branch.as_deref(), Some("agent-1"));
    let (sink, mut screen) = Screen::new();
    m.attach_pane(h, launched.pane.id, sink).await.unwrap();
    screen
        .until("agent output", |s| s.text().contains("agent running"))
        .await;
    assert!(
        m.list_panes(h)
            .await
            .unwrap()
            .iter()
            .any(|p| p.id == launched.pane.id)
    );

    // Linger: after detach the view stays cached, then is dropped.
    m.detach_pane(h, launched.pane.id).unwrap();
    assert!(m.with_view(h, launched.pane.id, |_| ()).is_some());
    eventually("linger expiry", || {
        m.with_view(h, launched.pane.id, |_| ()).is_none()
    })
    .await;

    m.kill_pane(h, launched.pane.id, None).await.unwrap();
    m.remove_worktree(h, wt.id.clone(), true).await.unwrap();
    m.remove_repo(h, repo.id).await.unwrap();
    assert!(m.list_repos(h).await.unwrap().is_empty());
    m.disconnect_host(h).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_restart_rebootstraps_with_reconnecting_only_after_1s() {
    let mut rig = Rig::new(fast()).await;
    let h = "local";
    rig.mgr
        .connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    let pane = rig.mgr.spawn_pane(h, shell_pane("exec cat")).await.unwrap();
    let (sink, mut screen) = Screen::new();
    rig.mgr.attach_pane(h, pane.id, sink).await.unwrap();
    screen.until("full", |s| !s.records.is_empty()).await;
    let first_instance = rig.mgr.list_hosts()[0].instance_id.clone();

    // Quick restart (well under 1 s): re-bootstrap, never "reconnecting".
    let before = rig.ui.states().len();
    rig.restart_daemon(Duration::ZERO).await;
    eventually("new instance", || {
        rig.mgr.list_hosts()[0].instance_id != first_instance
            && rig.mgr.list_hosts()[0].state == HostState::Connected
    })
    .await;
    let quick: Vec<HostState> = rig.ui.states()[before..].to_vec();
    assert!(
        !quick.contains(&HostState::Reconnecting),
        "sub-second recovery must be invisible: {quick:?}"
    );
    // The old daemon's panes are gone: the view got a pane-closed record.
    screen
        .until("pane closed", |s| {
            s.records
                .iter()
                .any(|(h, _)| h.kind == RecordKind::PaneClosed)
        })
        .await;

    // The new session's RTT is published again (prediction needs it).
    eventually("rtt after re-bootstrap", || {
        let st = rig.ui.states.lock().unwrap();
        st[before..]
            .iter()
            .rev()
            .find(|e| e.state == HostState::Connected)
            .is_some_and(|e| e.rtt_ms.is_some())
            && st.last().is_some_and(|e| e.rtt_ms.is_some())
    })
    .await;

    // Slow restart (1.5 s): connected → reconnecting (after 1 s) → connected.
    let before = rig.ui.states().len();
    rig.restart_daemon(Duration::from_millis(1500)).await;
    eventually("connected again", || {
        let s = rig.ui.states();
        s.len() > before
            && s[before..].contains(&HostState::Reconnecting)
            && *s.last().unwrap() == HostState::Connected
    })
    .await;
    let slow: Vec<HostState> = rig.ui.states()[before..].to_vec();
    let r = slow
        .iter()
        .position(|s| *s == HostState::Reconnecting)
        .unwrap();
    assert!(slow[r..].contains(&HostState::Connected), "{slow:?}");

    // Still fully usable.
    let pane = rig.mgr.spawn_pane(h, shell_pane("exec cat")).await.unwrap();
    let (sink, mut screen) = Screen::new();
    rig.mgr.attach_pane(h, pane.id, sink).await.unwrap();
    rig.mgr
        .write_input(h, pane.id, b"still alive\r".to_vec())
        .unwrap();
    screen
        .until("echo", |s| s.text().contains("still alive"))
        .await;
    rig.mgr.shutdown().await;
    if let Some(d) = rig.daemon.take() {
        d.shutdown("test done").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_failure_is_reported_as_failed() {
    let mut rig = Rig::new(fast()).await;
    if let Some(d) = rig.daemon.take() {
        d.shutdown("no daemon").await;
    }
    // The first key is consumed by nobody: the connector waits for a fresh
    // daemon.json and gives up.
    let err = rig
        .mgr
        .connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap_err();
    assert_eq!(err.code, "bootstrap");
    assert_eq!(rig.mgr.list_hosts()[0].state, HostState::Failed);
    assert_eq!(*rig.ui.states().last().unwrap(), HostState::Failed);
    let ssh_target = HostTarget::Ssh {
        destination: "   ".into(),
        port: None,
        extra_args: vec![],
    };
    assert_eq!(
        rig.mgr
            .connect_host(ssh_target, TransportChoice::Auto)
            .await
            .unwrap_err()
            .code,
        "invalidArgument"
    );
}

/// OSC 52 from a remote program reaches the UI sink as a clipboard write
/// (not as a daemon event), once, and not again after a re-attach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn osc52_clipboard_write_reaches_the_ui() {
    let rig = Rig::new(fast()).await;
    *rig.ui.focused.lock().unwrap() = true;
    let m = &rig.mgr;
    let h = "local";
    m.connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    let pane = m
        .spawn_pane(
            h,
            shell_pane(
                "read x; printf '\\033]52;c;?\\007\\033]52;c;Y2xpcCDinJM=\\007ready\\n'; exec cat",
            ),
        )
        .await
        .unwrap();
    let (sink, mut screen) = Screen::new();
    m.attach_pane(h, pane.id, sink).await.unwrap();
    screen
        .until("first record", |s| !s.records.is_empty())
        .await;
    m.write_input(h, pane.id, b"go\r".to_vec()).unwrap();
    screen.until("ready", |s| s.text().contains("ready")).await;
    eventually("clipboard write", || {
        !rig.ui.clipboard.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *rig.ui.clipboard.lock().unwrap(),
        vec![ClipboardWrite {
            host_id: h.into(),
            pane: pane.id,
            text: "clip ✓".into(),
        }]
    );
    assert!(
        !rig.ui
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e.event, civet_proto::Event::Clipboard { .. })),
        "clipboard text is not broadcast as a daemon event"
    );
    // Re-attach (full repaint): no second write.
    m.detach_pane(h, pane.id).unwrap();
    let (sink2, mut screen2) = Screen::new();
    m.attach_pane(h, pane.id, sink2).await.unwrap();
    screen2
        .until("repaint", |s| s.text().contains("ready"))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.ui.clipboard.lock().unwrap().len(), 1);
    m.shutdown().await;
}

/// OSC 52 policy: applied only while the window is focused and the pane is
/// attached in the UI, at most once per second per pane; everything else is
/// dropped silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn osc52_needs_focus_visibility_and_is_rate_limited() {
    let rig = Rig::new(ManagerConfig {
        detach_linger: Duration::from_secs(10),
        ..ManagerConfig::default()
    })
    .await;
    let m = &rig.mgr;
    let h = "local";
    m.connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    // Each line read prints one OSC 52 write of the line, then "ok <line>".
    let script = "while read x; do printf '\\033]52;c;%s\\007ok %s\\n' \"$(printf %s \"$x\" | base64)\" \"$x\"; done";
    let pane = m.spawn_pane(h, shell_pane(script)).await.unwrap();
    let (sink, mut screen) = Screen::new();
    m.attach_pane(h, pane.id, sink).await.unwrap();
    screen
        .until("first record", |s| !s.records.is_empty())
        .await;
    let writes = || {
        rig.ui
            .clipboard
            .lock()
            .unwrap()
            .iter()
            .map(|w| w.text.clone())
            .collect::<Vec<_>>()
    };

    // Window not focused: dropped.
    m.write_input(h, pane.id, b"one\r".to_vec()).unwrap();
    screen
        .until("ok one", |s| s.text().contains("ok one"))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(writes().is_empty(), "unfocused window");

    // Focused and attached: applied once; a second write within 1 s is
    // rate limited.
    *rig.ui.focused.lock().unwrap() = true;
    m.write_input(h, pane.id, b"two\r".to_vec()).unwrap();
    screen
        .until("ok two", |s| s.text().contains("ok two"))
        .await;
    m.write_input(h, pane.id, b"three\r".to_vec()).unwrap();
    screen
        .until("ok three", |s| s.text().contains("ok three"))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(writes(), vec!["two".to_string()], "rate limited");

    // After the interval it works again.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    m.write_input(h, pane.id, b"four\r".to_vec()).unwrap();
    eventually("four", || writes().len() == 2).await;
    assert_eq!(writes()[1], "four");

    // Detached (not visible in the UI) but still mirrored during the
    // linger: dropped.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    m.detach_pane(h, pane.id).unwrap();
    m.write_input(h, pane.id, b"five\r".to_vec()).unwrap();
    eventually("five mirrored", || {
        m.with_view(h, pane.id, |v| {
            v.predictor()
                .mirror()
                .screen_text()
                .join("\n")
                .contains("ok five")
        })
        .unwrap_or(false)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(writes().len(), 2, "detached pane");
    m.shutdown().await;
}

/// Detach, then re-attach right around the moment the detach linger expires:
/// the linger's unsubscribe must never cancel the new attach's subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reattach_racing_the_linger_expiry_keeps_streaming() {
    let linger = Duration::from_millis(150);
    let rig = Rig::new(ManagerConfig {
        detach_linger: linger,
        ..ManagerConfig::default()
    })
    .await;
    let m = &rig.mgr;
    let h = "local";
    m.connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    let pane = m.spawn_pane(h, shell_pane("exec cat")).await.unwrap();
    let (sink, mut screen) = Screen::new();
    m.attach_pane(h, pane.id, sink).await.unwrap();
    screen
        .until("first record", |s| !s.records.is_empty())
        .await;
    for (i, offset_ms) in [-6i64, -3, -1, 0, 1, 2, 3, 5, 8, 12]
        .into_iter()
        .enumerate()
    {
        m.detach_pane(h, pane.id).unwrap();
        let wait = linger.as_millis() as i64 + offset_ms;
        tokio::time::sleep(Duration::from_millis(wait.max(0) as u64)).await;
        let (sink, mut screen) = Screen::new();
        m.attach_pane(h, pane.id, sink).await.unwrap();
        let marker = format!("race-{i}");
        m.write_input(h, pane.id, format!("{marker}\r").into_bytes())
            .unwrap();
        screen
            .until(&marker, |s| s.text().matches(&marker).count() >= 2)
            .await;
        // Still subscribed a while later (a late unsubscribe would stop it).
        tokio::time::sleep(linger).await;
        let later = format!("later-{i}");
        m.write_input(h, pane.id, format!("{later}\r").into_bytes())
            .unwrap();
        screen
            .until(&later, |s| s.text().matches(&later).count() >= 2)
            .await;
    }
    m.shutdown().await;
}
