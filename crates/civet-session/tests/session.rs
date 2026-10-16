//! Integration tests: real PTYs, real git, real hook files.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use civet_proto::{
    AgentStatus, AgentStatusEvent, AgentStatusSource, ClientId, CommandSpec, Event, InputMessage,
    KillSignal, LaunchAgentRequest, MAX_INFLIGHT_UPDATES, PaneId, RepoId, SpawnPaneRequest,
    TermSize, UpdateKind, WorktreeId,
};
use civet_session::{InputOutcome, PaneOutput, SessionConfig, SessionError, SessionManager};
use tokio::sync::broadcast;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    mgr: SessionManager,
}

fn fixture() -> Fixture {
    fixture_with(Duration::from_millis(500))
}

fn fixture_with(idle_after: Duration) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut cfg = SessionConfig::new(root.join("state"));
    cfg.shell = Some("/bin/sh".into());
    cfg.idle_after = idle_after;
    let mgr = SessionManager::new(cfg).unwrap();
    Fixture {
        _dir: dir,
        root,
        mgr,
    }
}

fn client() -> ClientId {
    ClientId([7; 16])
}

fn req(program: &str, args: &[&str]) -> SpawnPaneRequest {
    SpawnPaneRequest {
        worktree: None,
        cwd: None,
        command: Some(CommandSpec::new(program, args.iter().copied())),
        env: vec![],
        size: TermSize::new(80, 24),
        title: None,
        agent: None,
    }
}

fn sh(script: &str) -> SpawnPaneRequest {
    req("sh", &["-c", script])
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < T, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_screen(mgr: &SessionManager, pane: PaneId, needle: &str) -> Vec<String> {
    let mut last = Vec::new();
    let start = Instant::now();
    loop {
        if let Ok(text) = mgr.screen_text(pane) {
            if text.iter().any(|l| l.contains(needle)) {
                return text;
            }
            last = text;
        }
        assert!(
            start.elapsed() < T,
            "timed out waiting for {needle:?} on screen: {last:#?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn next_event(
    rx: &mut broadcast::Receiver<Event>,
    mut pred: impl FnMut(&Event) -> bool,
) -> Event {
    let fut = async {
        loop {
            match rx.recv().await {
                Ok(e) if pred(&e) => return e,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("event channel closed"),
            }
        }
    };
    tokio::time::timeout(T, fut)
        .await
        .expect("timed out waiting for event")
}

async fn wait_status(
    rx: &mut broadcast::Receiver<Event>,
    pane: PaneId,
    status: AgentStatus,
) -> AgentStatusEvent {
    match next_event(
        rx,
        |e| matches!(e, Event::AgentStatus(s) if s.pane == pane && s.status == status),
    )
    .await
    {
        Event::AgentStatus(s) => s,
        _ => unreachable!(),
    }
}

fn input(pane: PaneId, seq: u64, data: &str) -> InputMessage {
    InputMessage {
        pane,
        seq,
        data: data.as_bytes().to_vec(),
    }
}

// ----- panes ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn spawn_echo_shows_output_and_exits() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let info = f.mgr.spawn_pane(sh("echo hi")).await.unwrap();
    assert_eq!(info.command, vec!["sh", "-c", "echo hi"]);
    assert_eq!(info.title, "sh");
    assert!(matches!(
        next_event(&mut events, |e| matches!(e, Event::PaneCreated(_))).await,
        Event::PaneCreated(p) if p.id == info.id
    ));
    let text = wait_screen(&f.mgr, info.id, "hi").await;
    assert!(text.iter().any(|l| l.trim() == "hi"), "{text:#?}");

    let exited = next_event(&mut events, |e| matches!(e, Event::PaneExited { .. })).await;
    assert_eq!(
        exited,
        Event::PaneExited {
            pane: info.id,
            exit_code: Some(0)
        }
    );
    let after = f.mgr.pane_info(info.id).unwrap();
    assert!(after.exited);
    assert_eq!(after.exit_code, Some(0));
    assert_eq!(after.status, AgentStatus::Done);
    // Exited panes stay readable, refuse input.
    assert!(f.mgr.screen_text(info.id).is_ok());
    assert!(matches!(
        f.mgr.write_input(client(), &input(info.id, 1, "x")),
        Err(SessionError::InvalidArgument(_))
    ));
    // A subscription still gets the final screen as a Full update.
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    match tokio::time::timeout(T, sub.next()).await.unwrap() {
        Some(PaneOutput::Screen(u)) => {
            assert_eq!(u.kind, UpdateKind::Full);
            assert_eq!(u.base_state, 0);
            assert!(String::from_utf8_lossy(&u.payload).contains("hi"));
        }
        other => panic!("expected a Full update, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn env_cwd_and_shell_panes() {
    let f = fixture();
    let cwd = f.root.join("work dir");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut r = sh(
        "echo \"pane=$CIVET_PANE_ID term=$TERM x=$EXTRA\"; pwd; test -f \"$CIVET_STATUS_FILE\" && echo status-ok; sleep 30",
    );
    r.cwd = Some(cwd.to_string_lossy().into_owned());
    r.env = vec![("EXTRA".into(), "yes".into())];
    r.title = Some("my title".into());
    let info = f.mgr.spawn_pane(r).await.unwrap();
    assert_eq!(info.title, "my title");
    assert_eq!(info.cwd, cwd.to_string_lossy());
    wait_screen(&f.mgr, info.id, "status-ok").await;
    let text = f.mgr.screen_text(info.id).unwrap().join("\n");
    assert!(
        text.contains(&format!("pane={} term=xterm-256color x=yes", info.id.0)),
        "{text}"
    );
    assert!(text.contains("work dir"), "{text}");

    // missing cwd
    let mut bad = sh("true");
    bad.cwd = Some("/definitely/not/here".into());
    assert!(matches!(
        f.mgr.spawn_pane(bad).await,
        Err(SessionError::InvalidArgument(_))
    ));
    // zero size
    let mut bad = sh("true");
    bad.size = TermSize::new(0, 10);
    assert!(matches!(
        f.mgr.spawn_pane(bad).await,
        Err(SessionError::InvalidArgument(_))
    ));

    // A plain login shell pane accepts input.
    let mut shell = req("x", &[]);
    shell.command = None;
    let p = f.mgr.spawn_pane(shell).await.unwrap();
    assert_eq!(p.command, vec!["/bin/sh", "-l"]);
    f.mgr
        .write_input(client(), &input(p.id, 1, "echo $((40+2))\n"))
        .unwrap();
    wait_screen(&f.mgr, p.id, "42").await;

    // Command string form (program with spaces, no args).
    let p = f
        .mgr
        .spawn_pane(req("echo one; echo two-$((1+1))", &[]))
        .await
        .unwrap();
    wait_screen(&f.mgr, p.id, "two-2").await;
    f.mgr.shutdown().await;
    assert!(f.mgr.list_panes().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn tcsh_runs_commands() {
    // tcsh rejects `-lc` (`-l` must be its only argument).
    let Some(tcsh) = ["/bin/tcsh", "/usr/bin/tcsh"]
        .into_iter()
        .find(|p| Path::new(p).exists())
    else {
        eprintln!("skipping: no tcsh");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = SessionConfig::new(dir.path().join("state"));
    cfg.shell = Some(tcsh.into());
    let mgr = SessionManager::new(cfg).unwrap();

    // Program + args: quoting survives the `$argv` exec.
    let p = mgr
        .spawn_pane(req("printf", &["[%s]\\n", "a b", "argv-ok"]))
        .await
        .unwrap();
    wait_screen(&mgr, p.id, "[argv-ok]").await;
    let text = mgr.screen_text(p.id).unwrap().join("\n");
    assert!(text.contains("[a b]"), "{text}");

    // Command string, with the shim dir first on PATH.
    let mut r = req("echo first=`echo $PATH | cut -d: -f1`; echo cmd-ok", &[]);
    r.size = TermSize::new(400, 24);
    let p = mgr.spawn_pane(r).await.unwrap();
    wait_screen(&mgr, p.id, "cmd-ok").await;
    let text = mgr.screen_text(p.id).unwrap().join("\n");
    assert!(text.contains("hooks/bin"), "{text}");
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_round_trip_with_cat() {
    let f = fixture();
    let info = f.mgr.spawn_pane(req("cat", &[])).await.unwrap();
    let out = f
        .mgr
        .write_input(client(), &input(info.id, 1, "hello\n"))
        .unwrap();
    assert_eq!(out, InputOutcome::Applied);
    wait_until("echo and cat output", || {
        f.mgr
            .screen_text(info.id)
            .unwrap()
            .iter()
            .filter(|l| l.trim() == "hello")
            .count()
            >= 2
    })
    .await;
    assert_eq!(f.mgr.last_input_seq(client(), info.id), 1);
    assert_eq!(f.mgr.last_input_seq(ClientId([9; 16]), info.id), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_input_seq_is_dropped() {
    let f = fixture();
    let info = f.mgr.spawn_pane(req("cat", &[])).await.unwrap();
    let c = client();
    assert_eq!(
        f.mgr.write_input(c, &input(info.id, 1, "first\n")).unwrap(),
        InputOutcome::Applied
    );
    assert_eq!(
        f.mgr
            .write_input(c, &input(info.id, 1, "DUPLICATE\n"))
            .unwrap(),
        InputOutcome::Duplicate
    );
    assert_eq!(
        f.mgr.write_input(c, &input(info.id, 0, "ZERO\n")).unwrap(),
        InputOutcome::Duplicate
    );
    // Another client has its own sequence space.
    let other = ClientId([8; 16]);
    assert_eq!(
        f.mgr
            .write_input(other, &input(info.id, 1, "other\n"))
            .unwrap(),
        InputOutcome::Applied
    );
    // Gaps are allowed.
    assert_eq!(
        f.mgr.write_input(c, &input(info.id, 3, "third\n")).unwrap(),
        InputOutcome::Applied
    );
    assert_eq!(
        f.mgr.write_input(c, &input(info.id, 2, "LATE\n")).unwrap(),
        InputOutcome::Duplicate
    );
    wait_screen(&f.mgr, info.id, "third").await;
    let text = f.mgr.screen_text(info.id).unwrap().join("\n");
    assert!(text.contains("first") && text.contains("other"), "{text}");
    assert!(!text.contains("DUPLICATE") && !text.contains("ZERO") && !text.contains("LATE"));
    assert_eq!(f.mgr.last_input_seq(c, info.id), 3);

    // A fresh (non-resumed) session starts numbering again.
    f.mgr.reset_client(c);
    assert_eq!(f.mgr.last_input_seq(c, info.id), 0);
    assert_eq!(
        f.mgr.write_input(c, &input(info.id, 1, "again\n")).unwrap(),
        InputOutcome::Applied
    );
    wait_screen(&f.mgr, info.id, "again").await;
    assert!(matches!(
        f.mgr.write_input(c, &input(PaneId(999), 5, "x")),
        Err(SessionError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_reaches_pty_and_terminal() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let info = f
        .mgr
        .spawn_pane(sh("while read l; do stty size; done"))
        .await
        .unwrap();
    f.mgr
        .write_input(client(), &input(info.id, 1, "\n"))
        .unwrap();
    wait_screen(&f.mgr, info.id, "24 80").await;

    let size = TermSize::new(100, 30);
    f.mgr.resize_pane(info.id, size).unwrap();
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::PaneResized { .. })).await,
        Event::PaneResized {
            pane: info.id,
            size
        }
    );
    assert_eq!(f.mgr.pane_info(info.id).unwrap().size, size);
    f.mgr
        .write_input(client(), &input(info.id, 2, "\n"))
        .unwrap();
    wait_screen(&f.mgr, info.id, "30 100").await;
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    assert_eq!(sub.info().size.cols, 100);
    assert!(matches!(
        f.mgr.resize_pane(info.id, TermSize::new(0, 0)),
        Err(SessionError::InvalidArgument(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_pane_closes_subscriptions() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let info = f.mgr.spawn_pane(sh("trap '' HUP; sleep 60")).await.unwrap();
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    assert!(matches!(
        tokio::time::timeout(T, sub.next()).await.unwrap(),
        Some(PaneOutput::Screen(_))
    ));
    let waiter = tokio::spawn({
        let sub = sub.clone();
        async move { sub.next().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    f.mgr.kill_pane(info.id, KillSignal::Terminate).unwrap();
    assert!(
        tokio::time::timeout(T, waiter)
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::PaneClosed { .. })).await,
        Event::PaneClosed { pane: info.id }
    );
    assert!(f.mgr.list_panes().iter().all(|p| p.id != info.id));
    assert!(matches!(
        f.mgr.kill_pane(info.id, KillSignal::Kill),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        f.mgr.subscribe(client(), info.id),
        Err(SessionError::NotFound(_))
    ));
}

// ----- subscriptions -------------------------------------------------------

async fn next_screen(sub: &civet_session::Subscription) -> civet_proto::ScreenUpdate {
    loop {
        match tokio::time::timeout(T, sub.next())
            .await
            .expect("next() timed out")
        {
            Some(PaneOutput::Screen(u)) => return u,
            Some(PaneOutput::InputAck { .. }) => {}
            None => panic!("pane closed"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_full_then_diffs_and_input_acks() {
    let f = fixture();
    let info = f.mgr.spawn_pane(req("cat", &[])).await.unwrap();
    let c = client();
    let sub = f.mgr.subscribe(c, info.id).unwrap();
    assert_eq!(sub.pane(), info.id);
    assert_eq!(sub.client(), c);
    let first = next_screen(&sub).await;
    assert_eq!(first.kind, UpdateKind::Full);
    sub.ack(first.state);

    f.mgr.write_input(c, &input(info.id, 1, "ping\n")).unwrap();
    // Eventually input seq 1 is acknowledged, by an update or an InputAck.
    let mut last_state = first.state;
    let start = Instant::now();
    let mut acked = false;
    while !acked {
        assert!(start.elapsed() < T, "input never acked");
        match tokio::time::timeout(T, sub.next()).await.unwrap().unwrap() {
            PaneOutput::Screen(u) => {
                assert_eq!(u.kind, UpdateKind::Diff);
                assert_eq!(u.base_state, last_state);
                assert!(u.state > last_state);
                last_state = u.state;
                sub.ack(u.state);
                acked = u.acked_input_seq >= 1;
            }
            PaneOutput::InputAck { pane, seq } => {
                assert_eq!(pane, info.id);
                acked = seq >= 1;
            }
        }
    }
    assert_eq!(sub.info().last_input_seq, 1);
    wait_screen(&f.mgr, info.id, "ping").await;

    // Resubscribe with a held (acked) state → Diff; unknown → Full.
    let cur = loop {
        let u = tokio::time::timeout(Duration::from_millis(300), sub.next()).await;
        match u {
            Ok(Some(PaneOutput::Screen(u))) => {
                sub.ack(u.state);
                last_state = u.state;
            }
            Ok(Some(PaneOutput::InputAck { .. })) => {}
            Ok(None) => panic!("closed"),
            Err(_) => break last_state,
        }
    };
    sub.reset_baseline(Some(cur));
    let u = next_screen(&sub).await;
    assert_eq!(u.kind, UpdateKind::Diff);
    assert_eq!(u.base_state, cur);
    sub.reset_baseline(Some(12345));
    assert_eq!(next_screen(&sub).await.kind, UpdateKind::Full);
    sub.reset_baseline(None);
    assert_eq!(next_screen(&sub).await.kind, UpdateKind::Full);
}

#[tokio::test(flavor = "multi_thread")]
async fn flow_control_window_and_coalescing() {
    let f = fixture();
    let info = f
        .mgr
        .spawn_pane(sh(
            "i=0; while [ $i -lt 150 ]; do echo line $i; i=$((i+1)); sleep 0.01; done; echo finished; sleep 30",
        ))
        .await
        .unwrap();
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    // Without acks, at most MAX_INFLIGHT_UPDATES updates are sent.
    let mut unacked = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(800), sub.next()).await {
            Ok(Some(PaneOutput::Screen(u))) => {
                unacked.push(u.state);
                assert!(sub.inflight() <= MAX_INFLIGHT_UPDATES);
                assert!(unacked.len() <= MAX_INFLIGHT_UPDATES, "window exceeded");
            }
            Ok(Some(PaneOutput::InputAck { .. })) => {}
            Ok(None) => panic!("closed"),
            Err(_) => break,
        }
    }
    assert_eq!(unacked.len(), MAX_INFLIGHT_UPDATES, "window should fill");
    wait_screen(&f.mgr, info.id, "finished").await;
    // Acking reopens the window; the next update jumps to the newest state
    // (newest wins) relative to the last sent one.
    sub.ack(*unacked.last().unwrap());
    let u = next_screen(&sub).await;
    assert_eq!(u.base_state, *unacked.last().unwrap());
    assert_eq!(u.state, sub.info().state, "newest state is sent");
    assert!(String::from_utf8_lossy(&u.payload).contains("finished"));
}

#[tokio::test(flavor = "multi_thread")]
async fn coalescing_under_heavy_output() {
    let f = fixture();
    let info = f
        .mgr
        .spawn_pane(sh("yes | head -c 400000; echo; echo done-yes; sleep 30"))
        .await
        .unwrap();
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    let mut updates = 0u64;
    let start = Instant::now();
    loop {
        let u = next_screen(&sub).await;
        sub.ack(u.state);
        updates += 1;
        if f.mgr
            .screen_text(info.id)
            .unwrap()
            .iter()
            .any(|l| l == "done-yes")
            && u.state == sub.info().state
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "too slow");
    }
    let states = sub.info().state;
    assert!(
        updates < states,
        "updates ({updates}) should be coalesced below states ({states})"
    );
}

// ----- git -----------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn make_repo(root: &Path) -> PathBuf {
    let repo = root.join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

#[tokio::test(flavor = "multi_thread")]
async fn repos_and_worktrees() {
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    std::fs::create_dir_all(repo_dir.join("sub")).unwrap();
    let mut events = f.mgr.subscribe_events();

    let repo = f
        .mgr
        .add_repo(&repo_dir.join("sub").to_string_lossy())
        .await
        .unwrap();
    assert_eq!(repo.name, "proj");
    assert_eq!(repo.path, repo_dir.to_string_lossy());
    assert_eq!(repo.default_branch.as_deref(), Some("main"));
    assert_eq!(repo.id.0.len(), 16);
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::ReposChanged)).await,
        Event::ReposChanged
    );
    assert!(matches!(
        f.mgr.add_repo(&repo_dir.to_string_lossy()).await,
        Err(SessionError::AlreadyExists(_))
    ));
    assert!(matches!(
        f.mgr.add_repo(&f.root.to_string_lossy()).await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert_eq!(f.mgr.list_repos().await.unwrap(), vec![repo.clone()]);

    let wts = f.mgr.list_worktrees(&repo.id).await.unwrap();
    assert_eq!(wts.len(), 1);
    assert!(wts[0].is_main);
    assert_eq!(wts[0].branch.as_deref(), Some("main"));
    assert!(wts[0].head.is_some());

    // New branch from HEAD at the default location.
    let wt = f
        .mgr
        .create_worktree(&repo.id, "feat/x", None, None)
        .await
        .unwrap();
    let expected = f.root.join("proj-worktrees").join("feat-x");
    assert_eq!(wt.path, expected.to_string_lossy());
    assert_eq!(wt.id, WorktreeId(wt.path.clone()));
    assert_eq!(wt.branch.as_deref(), Some("feat/x"));
    assert!(!wt.is_main);
    assert!(expected.join("README").exists());
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::WorktreesChanged { .. })).await,
        Event::WorktreesChanged {
            repo: repo.id.clone()
        }
    );

    // Existing branch, explicit path: checked out.
    git(&repo_dir, &["branch", "existing"]);
    let other = f.root.join("elsewhere");
    let wt2 = f
        .mgr
        .create_worktree(&repo.id, "existing", None, Some(&other.to_string_lossy()))
        .await
        .unwrap();
    assert_eq!(wt2.branch.as_deref(), Some("existing"));
    // From a base: branch must be new.
    assert!(matches!(
        f.mgr
            .create_worktree(&repo.id, "existing", Some("main"), None)
            .await,
        Err(SessionError::AlreadyExists(_))
    ));
    let wt3 = f
        .mgr
        .create_worktree(&repo.id, "from-base", Some("main"), None)
        .await
        .unwrap();
    assert_eq!(wt3.head, wts[0].head);
    assert!(matches!(
        f.mgr
            .create_worktree(&repo.id, "bad..name", None, None)
            .await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(matches!(
        f.mgr
            .create_worktree(&RepoId("nope".into()), "x", None, None)
            .await,
        Err(SessionError::NotFound(_))
    ));
    assert_eq!(f.mgr.list_worktrees(&repo.id).await.unwrap().len(), 4);

    // Remove: main refused, others removed.
    assert!(matches!(
        f.mgr.remove_worktree(&wts[0].id, false).await,
        Err(SessionError::InvalidArgument(_))
    ));
    std::fs::write(expected.join("dirty.txt"), "x").unwrap();
    assert!(matches!(
        f.mgr.remove_worktree(&wt.id, false).await,
        Err(SessionError::Git(_))
    ));
    f.mgr.remove_worktree(&wt.id, true).await.unwrap();
    f.mgr.remove_worktree(&wt2.id, false).await.unwrap();
    f.mgr.remove_worktree(&wt3.id, false).await.unwrap();
    assert!(!expected.exists());
    assert_eq!(f.mgr.list_worktrees(&repo.id).await.unwrap().len(), 1);
    assert!(matches!(
        f.mgr.remove_worktree(&wt.id, false).await,
        Err(SessionError::NotFound(_))
    ));

    // Persistence: a new manager on the same state dir knows the repo.
    let mut cfg = SessionConfig::new(f.root.join("state"));
    cfg.shell = Some("/bin/sh".into());
    let mgr2 = SessionManager::new(cfg).unwrap();
    assert_eq!(mgr2.list_repos().await.unwrap(), vec![repo.clone()]);
    mgr2.remove_repo(&repo.id).await.unwrap();
    assert!(mgr2.list_repos().await.unwrap().is_empty());
    assert!(matches!(
        mgr2.remove_repo(&repo.id).await,
        Err(SessionError::NotFound(_))
    ));
}

/// Worktrees changed behind the daemon's back: a deleted directory (still
/// registered, "prunable") and a locked one.
#[tokio::test(flavor = "multi_thread")]
async fn worktrees_changed_outside_civet() {
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();

    // `rm -rf` of a worktree: no longer listed, still removable by its id.
    let wt = f
        .mgr
        .create_worktree(&repo.id, "gone", None, None)
        .await
        .unwrap();
    std::fs::remove_dir_all(&wt.path).unwrap();
    let listed = f.mgr.list_worktrees(&repo.id).await.unwrap();
    assert!(!listed.iter().any(|w| w.id == wt.id), "{listed:?}");
    let mut events = f.mgr.subscribe_events();
    f.mgr.remove_worktree(&wt.id, false).await.unwrap();
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::WorktreesChanged { .. })).await,
        Event::WorktreesChanged {
            repo: repo.id.clone()
        }
    );
    assert_eq!(f.mgr.list_worktrees(&repo.id).await.unwrap().len(), 1);

    // A deleted directory no longer blocks re-creating its path or branch.
    let again = f
        .mgr
        .create_worktree(&repo.id, "stale", None, None)
        .await
        .unwrap();
    std::fs::remove_dir_all(&again.path).unwrap();
    let again2 = f
        .mgr
        .create_worktree(&repo.id, "stale", None, None)
        .await
        .unwrap();
    assert_eq!(again2.id, again.id);
    assert!(Path::new(&again2.path).join("README").exists());

    // Locked: a plain remove is refused, "Remove anyway" (force) works.
    git(&repo_dir, &["worktree", "lock", &again2.path]);
    let listed = f.mgr.list_worktrees(&repo.id).await.unwrap();
    assert!(listed.iter().any(|w| w.id == again2.id && w.locked));
    assert!(matches!(
        f.mgr.remove_worktree(&again2.id, false).await,
        Err(SessionError::Git(_))
    ));
    f.mgr.remove_worktree(&again2.id, true).await.unwrap();
    assert!(!Path::new(&again2.path).exists());
    assert_eq!(f.mgr.list_worktrees(&repo.id).await.unwrap().len(), 1);
}

/// Removing a worktree keeps its branch; creating the same name again from
/// the same base reuses the untouched branch, but never one with commits.
#[tokio::test(flavor = "multi_thread")]
async fn recreate_after_remove_reuses_untouched_branch() {
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f
        .mgr
        .create_worktree(&repo.id, "task", Some("main"), None)
        .await
        .unwrap();
    f.mgr.remove_worktree(&wt.id, false).await.unwrap();
    let again = f
        .mgr
        .create_worktree(&repo.id, "task", Some("main"), None)
        .await
        .unwrap();
    assert_eq!(again.branch.as_deref(), Some("task"));
    // Checked out in `again`: refused.
    assert!(matches!(
        f.mgr
            .create_worktree(&repo.id, "task", Some("main"), Some("other"))
            .await,
        Err(SessionError::AlreadyExists(_))
    ));
    // With a commit of its own: refused even when not checked out.
    git(
        Path::new(&again.path),
        &["commit", "-q", "--allow-empty", "-m", "work"],
    );
    f.mgr.remove_worktree(&again.id, false).await.unwrap();
    assert!(matches!(
        f.mgr
            .create_worktree(&repo.id, "task", Some("main"), None)
            .await,
        Err(SessionError::AlreadyExists(_))
    ));
}

/// `git worktree add` in a terminal reaches clients as WorktreesChanged.
#[tokio::test(flavor = "multi_thread")]
async fn external_worktree_changes_emit_events() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut cfg = SessionConfig::new(root.join("state"));
    cfg.shell = Some("/bin/sh".into());
    cfg.worktree_poll = Duration::from_millis(100);
    let mgr = SessionManager::new(cfg).unwrap();
    let repo_dir = make_repo(&root);
    let repo = mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    // Let the watcher take its first look at the new repo.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut events = mgr.subscribe_events();
    let wt = root.join("by-hand");
    git(
        &repo_dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "by-hand",
            &wt.to_string_lossy(),
        ],
    );
    assert_eq!(
        next_event(&mut events, |e| matches!(e, Event::WorktreesChanged { .. })).await,
        Event::WorktreesChanged {
            repo: repo.id.clone()
        }
    );
    let listed = mgr.list_worktrees(&repo.id).await.unwrap();
    assert!(
        listed
            .iter()
            .any(|w| w.branch.as_deref() == Some("by-hand"))
    );
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_agent_in_new_worktree() {
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let bin = f.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let agent = bin.join("fake-agent");
    write_script(
        &agent,
        "#!/bin/sh\necho \"fake agent args=$*\"\necho \"wt=$CIVET_WORKTREE\"\npwd\necho running > \"$CIVET_STATUS_FILE\"\nsleep 1\necho done > \"$CIVET_STATUS_FILE\"\nsleep 30\n",
    );
    let mut events = f.mgr.subscribe_events();
    let request = LaunchAgentRequest {
        repo: repo.id.clone(),
        branch: "agent/one".into(),
        base: None,
        worktree_path: None,
        command: CommandSpec::new(agent.to_string_lossy(), ["--yolo"]),
        env: vec![],
        size: TermSize::new(100, 30),
        title: None,
        agent: Some("fake".into()),
    };
    let (wt, pane) = f.mgr.launch_agent(request.clone()).await.unwrap();
    assert_eq!(wt.branch.as_deref(), Some("agent/one"));
    assert_eq!(pane.worktree, Some(wt.id.clone()));
    assert_eq!(pane.cwd, wt.path);
    assert_eq!(pane.title, "agent/one");
    assert_eq!(pane.agent.as_deref(), Some("fake"));
    let text = wait_screen(&f.mgr, pane.id, "fake agent args=--yolo").await;
    let joined = text.join("\n");
    assert!(joined.contains(&format!("wt={}", wt.path)), "{joined}");
    // Output may first flip it to Running heuristically; the hook then takes over.
    next_event(&mut events, |e| {
        matches!(e, Event::AgentStatus(s) if s.pane == pane.id
            && s.source == AgentStatusSource::Hook
            && s.status == AgentStatus::Done)
    })
    .await;
    assert_eq!(f.mgr.pane_info(pane.id).unwrap().status, AgentStatus::Done);

    // Same branch again → AlreadyExists.
    assert!(matches!(
        f.mgr.launch_agent(request.clone()).await,
        Err(SessionError::AlreadyExists(_))
    ));

    // A program named `claude` gets the generated hook settings; a command
    // string works too.
    let claude = bin.join("claude");
    write_script(&claude, "#!/bin/sh\necho \"claude args: $*\"\nsleep 30\n");
    let (_, p) = f
        .mgr
        .launch_agent(LaunchAgentRequest {
            branch: "agent/two".into(),
            base: Some("main".into()),
            command: CommandSpec::new(claude.to_string_lossy(), ["-p", "hi"]),
            agent: None,
            ..request.clone()
        })
        .await
        .unwrap();
    let settings = f.root.join("state/hooks/claude-settings.json");
    assert!(settings.exists());
    let text = wait_screen(&f.mgr, p.id, "claude args:").await.join("");
    assert!(
        text.contains("--settings") && text.contains("claude-settings.json"),
        "{text}"
    );
    assert_eq!(p.command.last().unwrap(), &settings.to_string_lossy());

    let (_, p) = f
        .mgr
        .launch_agent(LaunchAgentRequest {
            branch: "agent/three".into(),
            command: CommandSpec::new("echo via-string && sleep 30", Vec::<String>::new()),
            agent: None,
            title: Some("t3".into()),
            ..request
        })
        .await
        .unwrap();
    assert_eq!(p.title, "t3");
    wait_screen(&f.mgr, p.id, "via-string").await;
}

// ----- agent status --------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn status_idle_heuristic() {
    let f = fixture_with(Duration::from_millis(400));
    let mut events = f.mgr.subscribe_events();
    let mut r = sh("sleep 0.2; printf '%0200d\\n' 0; sleep 30");
    r.agent = Some("fake".into());
    let info = f.mgr.spawn_pane(r).await.unwrap();
    assert_eq!(info.status, AgentStatus::Unknown);
    let s = wait_status(&mut events, info.id, AgentStatus::Running).await;
    assert_eq!(s.source, AgentStatusSource::Heuristic);
    assert_eq!(s.previous, AgentStatus::Unknown);
    let s = wait_status(&mut events, info.id, AgentStatus::Idle).await;
    assert_eq!(s.source, AgentStatusSource::Heuristic);
    assert_eq!(s.previous, AgentStatus::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_bell_then_quiet_needs_input() {
    let f = fixture_with(Duration::from_secs(60));
    let mut events = f.mgr.subscribe_events();
    // Known agent basename without an explicit hint.
    let bin = f.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    write_script(
        &codex,
        "#!/bin/sh\nsleep 0.2\nprintf '%0200d\\n' 0\nprintf 'Approve? \\007'\nsleep 30\n",
    );
    let info = f
        .mgr
        .spawn_pane(req(&codex.to_string_lossy(), &[]))
        .await
        .unwrap();
    wait_status(&mut events, info.id, AgentStatus::Running).await;
    assert!(matches!(
        next_event(&mut events, |e| matches!(e, Event::Bell { .. })).await,
        Event::Bell { pane } if pane == info.id
    ));
    let s = wait_status(&mut events, info.id, AgentStatus::NeedsInput).await;
    assert_eq!(s.source, AgentStatusSource::Heuristic);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_from_osc() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let info = f
        .mgr
        .spawn_pane(sh(
            "printf '\\033]133;C\\007'; sleep 0.3; printf '\\033]9;Task complete\\007'; sleep 0.3; printf '\\033]777;notify;Claude;Allow edit?\\007'; sleep 0.3; printf '\\033]133;D;0\\007'; sleep 30",
        ))
        .await
        .unwrap();
    let s = wait_status(&mut events, info.id, AgentStatus::Running).await;
    assert_eq!(s.source, AgentStatusSource::EscapeCode);
    let s = wait_status(&mut events, info.id, AgentStatus::Done).await;
    assert_eq!(s.source, AgentStatusSource::EscapeCode);
    assert_eq!(s.message.as_deref(), Some("Task complete"));
    let s = wait_status(&mut events, info.id, AgentStatus::NeedsInput).await;
    assert_eq!(s.message.as_deref(), Some("Allow edit?"));
    let s = wait_status(&mut events, info.id, AgentStatus::Idle).await;
    assert_eq!(s.source, AgentStatusSource::EscapeCode);

    // Client override.
    f.mgr
        .set_agent_status(info.id, AgentStatus::Running)
        .unwrap();
    let s = wait_status(&mut events, info.id, AgentStatus::Running).await;
    assert_eq!(s.source, AgentStatusSource::Client);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_from_hook_file_disables_heuristics() {
    let f = fixture_with(Duration::from_millis(300));
    let mut events = f.mgr.subscribe_events();
    let mut r = sh(
        "sleep 0.2; printf 'needs_input\\nAllow Bash?\\n' > \"$CIVET_STATUS_FILE\"; sleep 0.5; printf '{\"status\":\"running\"}' > \"$CIVET_STATUS_FILE\"; printf '%0200d\\n' 0; sleep 30",
    );
    r.agent = Some("fake".into());
    let info = f.mgr.spawn_pane(r).await.unwrap();
    let s = wait_status(&mut events, info.id, AgentStatus::NeedsInput).await;
    assert_eq!(s.source, AgentStatusSource::Hook);
    assert_eq!(s.message.as_deref(), Some("Allow Bash?"));
    let s = wait_status(&mut events, info.id, AgentStatus::Running).await;
    assert_eq!(s.source, AgentStatusSource::Hook);
    // The idle heuristic (300 ms) must not fire for a hooked pane.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        f.mgr.pane_info(info.id).unwrap().status,
        AgentStatus::Running
    );
    while let Ok(e) = events.try_recv() {
        if let Event::AgentStatus(s) = e {
            assert_ne!(s.source, AgentStatusSource::Heuristic, "{s:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn status_process_exit_is_done() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let mut r = sh("printf '%0200d\\n' 0; exit 3");
    r.agent = Some("fake".into());
    let info = f.mgr.spawn_pane(r).await.unwrap();
    let s = wait_status(&mut events, info.id, AgentStatus::Done).await;
    assert_eq!(s.source, AgentStatusSource::ProcessExit);
    assert_eq!(s.message.as_deref(), Some("exit 3"));
    let p = f.mgr.pane_info(info.id).unwrap();
    assert!(p.exited);
    assert_eq!(p.exit_code, Some(3));
    assert_eq!(p.status, AgentStatus::Done);
}

#[tokio::test(flavor = "multi_thread")]
async fn osc52_clipboard_write_is_an_event() {
    let f = fixture();
    let mut events = f.mgr.subscribe_events();
    let info = f
        .mgr
        .spawn_pane(sh(
            "sleep 0.2; printf '\\033]52;c;?\\007'; printf '\\033]52;c;Y29waWVk\\007'; sleep 30",
        ))
        .await
        .unwrap();
    match next_event(&mut events, |e| matches!(e, Event::Clipboard { .. })).await {
        Event::Clipboard { pane, text } => {
            assert_eq!(pane, info.id);
            assert_eq!(text, "copied");
        }
        other => panic!("unexpected {other:?}"),
    }
}
