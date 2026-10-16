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
        .spawn_pane(sh("stty -echo; echo ready; while IFS= read -r line; do \
             case \"$line\" in finish) seq 1 150; echo finished ;; \
             *) echo \"$line\" ;; esac; done"))
        .await
        .unwrap();
    wait_screen(&f.mgr, info.id, "ready").await;
    let sub = f.mgr.subscribe(client(), info.id).unwrap();
    // Produce each update only after receiving the previous one, so filling
    // the window does not depend on shell sleeps or the runner's scheduling.
    let mut unacked = Vec::new();
    for i in 0..MAX_INFLIGHT_UPDATES {
        if i > 0 {
            f.mgr
                .write_input(client(), &input(info.id, i as u64, &format!("line {i}\n")))
                .unwrap();
        }
        unacked.push(next_screen(&sub).await.state);
        assert_eq!(sub.inflight(), unacked.len());
    }
    // Keep producing while the window is full: the PTY must still drain,
    // but the subscriber must not receive another screen until it acks.
    f.mgr
        .write_input(
            client(),
            &input(info.id, MAX_INFLIGHT_UPDATES as u64, "finish\n"),
        )
        .unwrap();
    wait_screen(&f.mgr, info.id, "finished").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), next_screen(&sub))
            .await
            .is_err(),
        "window exceeded"
    );
    assert_eq!(sub.inflight(), MAX_INFLIGHT_UPDATES);
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

#[tokio::test(flavor = "multi_thread")]
async fn browse_worktree_files() {
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    std::fs::write(repo_dir.join(".gitignore"), "target/\n*.log\n").unwrap();
    std::fs::create_dir_all(repo_dir.join("src/bin")).unwrap();
    std::fs::create_dir_all(repo_dir.join("target")).unwrap();
    std::fs::write(repo_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo_dir.join("target/out"), "x").unwrap();
    std::fs::write(repo_dir.join("debug.log"), "x").unwrap();
    std::fs::write(repo_dir.join("Notes.md"), "n").unwrap();
    std::fs::write(repo_dir.join("blob.bin"), b"\x89PNG\0\0").unwrap();
    // Tracked although it matches *.log: still listed.
    std::fs::write(repo_dir.join("kept.log"), "k").unwrap();
    git(&repo_dir, &["add", "-f", "kept.log"]);
    std::fs::write(f.root.join("secret"), "s").unwrap();
    std::os::unix::fs::symlink(f.root.join("secret"), repo_dir.join("escape")).unwrap();
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();

    let names =
        |l: &civet_proto::DirListing| l.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    let top = f.mgr.list_files(&wt, "", false).await.unwrap();
    assert_eq!(
        names(&top),
        [
            "src",
            ".gitignore",
            "blob.bin",
            "escape",
            "kept.log",
            "Notes.md",
            "README"
        ]
    );
    assert!(!top.truncated);
    let escape = top.entries.iter().find(|e| e.name == "escape").unwrap();
    assert_eq!(escape.kind, civet_proto::FileKind::Symlink);
    let readme = top.entries.iter().find(|e| e.name == "README").unwrap();
    assert_eq!((readme.kind, readme.size), (civet_proto::FileKind::File, 3));

    let all = f.mgr.list_files(&wt, ".", true).await.unwrap();
    let flagged: Vec<_> = all
        .entries
        .iter()
        .filter(|e| e.ignored)
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(flagged, ["target", "debug.log"]);
    let src = f.mgr.list_files(&wt, "src", false).await.unwrap();
    assert_eq!(names(&src), ["bin", "main.rs"]);
    // Inside an ignored directory, everything is flagged.
    let target = f.mgr.list_files(&wt, "target", true).await.unwrap();
    assert!(target.entries.iter().all(|e| e.ignored));

    let p = f.mgr.read_file(&wt, "src/main.rs", 1024).await.unwrap();
    assert_eq!(p.text, "fn main() {}\n");
    assert!(!p.binary && !p.truncated);
    let p = f.mgr.read_file(&wt, "src/main.rs", 4).await.unwrap();
    assert_eq!((p.text.as_str(), p.truncated, p.size), ("fn m", true, 13));
    let p = f.mgr.read_file(&wt, "blob.bin", 1024).await.unwrap();
    assert!(p.binary && p.text.is_empty());

    // Nothing outside the worktree, however it is spelled.
    for bad in ["..", "../secret", "src/../../secret", "/etc", "escape"] {
        assert!(
            matches!(
                f.mgr.list_files(&wt, bad, true).await,
                Err(SessionError::InvalidArgument(_))
            ),
            "list {bad:?}"
        );
        assert!(
            matches!(
                f.mgr.read_file(&wt, bad, 1024).await,
                Err(SessionError::InvalidArgument(_))
            ),
            "read {bad:?}"
        );
    }
    assert!(matches!(
        f.mgr.read_file(&wt, "src", 1024).await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(matches!(
        f.mgr.list_files(&wt, "missing", false).await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        f.mgr
            .list_files(&WorktreeId(f.root.to_string_lossy().into()), "", false)
            .await,
        Err(SessionError::NotFound(_))
    ));

    // A nested repository (like a submodule checkout) uses its own rules;
    // checking its entries from the outer repository would fail.
    let nested = repo_dir.join("vendor/lib");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    std::fs::write(nested.join(".gitignore"), "out/\n").unwrap();
    std::fs::create_dir(nested.join("out")).unwrap();
    std::fs::write(nested.join("lib.rs"), "").unwrap();
    let inner = f.mgr.list_files(&wt, "vendor/lib", false).await.unwrap();
    assert_eq!(names(&inner), [".gitignore", "lib.rs"]);
    std::fs::remove_dir_all(repo_dir.join("vendor")).unwrap();

    let files = f.mgr.list_all_files(&wt).await.unwrap();
    assert_eq!(
        files.paths,
        [
            ".gitignore",
            "Notes.md",
            "README",
            "blob.bin",
            "escape",
            "kept.log",
            "src/main.rs"
        ]
    );
    assert!(!files.truncated);
}

#[tokio::test(flavor = "multi_thread")]
async fn change_worktree_files() {
    use civet_proto::{FileOp, MAX_UPLOAD_BYTES};
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    std::fs::create_dir_all(repo_dir.join("src")).unwrap();
    std::fs::write(repo_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(f.root.join("secret"), "s").unwrap();
    std::os::unix::fs::symlink(f.root.join("secret"), repo_dir.join("escape")).unwrap();
    std::os::unix::fs::symlink(repo_dir.join(".git"), repo_dir.join("gitlink")).unwrap();
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    let op = |op: FileOp| f.mgr.file_op(&wt, op);
    let s = |v: &str| v.to_owned();
    let read = |p: &str| std::fs::read_to_string(repo_dir.join(p)).unwrap();
    let gone = |p: &str| std::fs::symlink_metadata(repo_dir.join(p)).is_err();

    // Create.
    let r = op(FileOp::CreateFile {
        path: s("./src//new.rs"),
    })
    .await
    .unwrap();
    assert_eq!((r.path.as_str(), r.trash_id), ("src/new.rs", None));
    assert_eq!(read("src/new.rs"), "");
    op(FileOp::CreateDir { path: s("docs") }).await.unwrap();
    assert!(repo_dir.join("docs").is_dir());
    op(FileOp::Write {
        path: s("docs/a.md"),
        data: b"# A".to_vec(),
    })
    .await
    .unwrap();
    assert_eq!(read("docs/a.md"), "# A");
    assert!(matches!(
        op(FileOp::CreateFile {
            path: s("nowhere/x")
        })
        .await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        op(FileOp::CreateFile {
            path: s("README/x")
        })
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(matches!(
        op(FileOp::Write {
            path: s("big"),
            data: vec![0; MAX_UPLOAD_BYTES + 1]
        })
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(gone("big"));

    // Nothing is overwritten.
    for o in [
        FileOp::CreateFile { path: s("README") },
        FileOp::CreateDir { path: s("src") },
        FileOp::Write {
            path: s("README"),
            data: b"x".to_vec(),
        },
        FileOp::Move {
            from: s("docs/a.md"),
            to: s("README"),
        },
        FileOp::Copy {
            from: s("src"),
            to: s("docs"),
        },
        FileOp::Move {
            from: s("README"),
            to: s("README"),
        },
    ] {
        assert!(
            matches!(op(o.clone()).await, Err(SessionError::AlreadyExists(_))),
            "{o:?}"
        );
    }
    assert_eq!(read("README"), "hi\n");
    assert_eq!(read("docs/a.md"), "# A");

    // Move / rename.
    op(FileOp::Move {
        from: s("docs/a.md"),
        to: s("src/a.md"),
    })
    .await
    .unwrap();
    assert!(gone("docs/a.md"));
    assert_eq!(read("src/a.md"), "# A");
    op(FileOp::Move {
        from: s("docs"),
        to: s("src/docs"),
    })
    .await
    .unwrap();
    assert!(repo_dir.join("src/docs").is_dir() && gone("docs"));
    assert!(matches!(
        op(FileOp::Move {
            from: s("src"),
            to: s("src/docs/src")
        })
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    // Case-only renames work on case-insensitive file systems too.
    op(FileOp::Move {
        from: s("src/a.md"),
        to: s("src/A.md"),
    })
    .await
    .unwrap();
    let names: Vec<_> = std::fs::read_dir(repo_dir.join("src"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.contains(&s("A.md")) && !names.contains(&s("a.md")),
        "{names:?}"
    );
    op(FileOp::Move {
        from: s("src/A.md"),
        to: s("src/a.md"),
    })
    .await
    .unwrap();
    // A symlink moves as itself (its target stays put).
    op(FileOp::Move {
        from: s("escape"),
        to: s("src/escape"),
    })
    .await
    .unwrap();
    assert!(
        std::fs::symlink_metadata(repo_dir.join("src/escape"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(f.root.join("secret")).unwrap(), "s");
    assert!(matches!(
        op(FileOp::Move {
            from: s("missing"),
            to: s("x")
        })
        .await,
        Err(SessionError::NotFound(_))
    ));

    // Copy (a tree, with a symlink kept as a link).
    std::os::unix::fs::symlink("main.rs", repo_dir.join("src/link")).unwrap();
    op(FileOp::Copy {
        from: s("src"),
        to: s("src2"),
    })
    .await
    .unwrap();
    assert_eq!(read("src2/main.rs"), "fn main() {}\n");
    assert_eq!(read("src2/a.md"), "# A");
    assert_eq!(
        std::fs::read_link(repo_dir.join("src2/link")).unwrap(),
        Path::new("main.rs")
    );
    assert!(matches!(
        op(FileOp::Copy {
            from: s("src"),
            to: s("src/docs/again")
        })
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    op(FileOp::Copy {
        from: s("README"),
        to: s("README copy"),
    })
    .await
    .unwrap();
    assert_eq!(read("README copy"), "hi\n");

    // Trash and restore, round trip.
    let t = op(FileOp::Trash { path: s("src2") }).await.unwrap();
    let id = t.trash_id.clone().unwrap();
    assert!(gone("src2"));
    let trash = f.root.join("state/trash");
    assert!(trash.join(&id).join("item/main.rs").is_file());
    let r = op(FileOp::Restore {
        trash_id: id.clone(),
    })
    .await
    .unwrap();
    assert_eq!(r.path, "src2");
    assert_eq!(read("src2/main.rs"), "fn main() {}\n");
    assert!(!trash.join(&id).exists());
    assert!(matches!(
        op(FileOp::Restore { trash_id: id }).await,
        Err(SessionError::NotFound(_))
    ));
    // Restoring onto a path that was taken again is refused, and the entry
    // stays restorable.
    let t = op(FileOp::Trash {
        path: s("README copy"),
    })
    .await
    .unwrap();
    let id = t.trash_id.unwrap();
    op(FileOp::CreateFile {
        path: s("README copy"),
    })
    .await
    .unwrap();
    assert!(matches!(
        op(FileOp::Restore {
            trash_id: id.clone()
        })
        .await,
        Err(SessionError::AlreadyExists(_))
    ));
    op(FileOp::Trash {
        path: s("README copy"),
    })
    .await
    .unwrap();
    op(FileOp::Restore { trash_id: id }).await.unwrap();
    assert_eq!(read("README copy"), "hi\n");
    for bad in ["../x", "1-2-3", ""] {
        assert!(matches!(
            op(FileOp::Restore { trash_id: s(bad) }).await,
            Err(SessionError::NotFound(_))
        ));
    }

    // Nothing outside the worktree, the root itself or `.git`.
    let paths = [
        "..",
        "../secret",
        "src/../../x",
        "/tmp/x",
        "",
        ".",
        ".git",
        ".git/config",
        ".GIT/HEAD",
        "src/.git",
        "gitlink/config",
        "src/escape/x",
    ];
    for bad in paths {
        for o in [
            FileOp::CreateFile { path: s(bad) },
            FileOp::CreateDir { path: s(bad) },
            FileOp::Write {
                path: s(bad),
                data: vec![],
            },
            FileOp::Trash { path: s(bad) },
            FileOp::Move {
                from: s(bad),
                to: s("moved"),
            },
            FileOp::Move {
                from: s("README"),
                to: s(bad),
            },
            FileOp::Copy {
                from: s(bad),
                to: s("copied"),
            },
            FileOp::Copy {
                from: s("README"),
                to: s(bad),
            },
        ] {
            let r = op(o.clone()).await;
            assert!(
                matches!(
                    r,
                    Err(SessionError::InvalidArgument(_) | SessionError::AlreadyExists(_))
                ),
                "{o:?}: {r:?}"
            );
        }
    }
    assert!(repo_dir.join(".git/HEAD").is_file());
    assert!(gone("moved") && gone("copied"));
    assert_eq!(read("README"), "hi\n");
    // A nested repository (its own .git) is neither moved nor trashed, and a
    // copy of it leaves the .git out.
    let nested = repo_dir.join("vendor");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    std::fs::write(nested.join("lib.rs"), "").unwrap();
    assert!(matches!(
        op(FileOp::Trash { path: s("vendor") }).await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(matches!(
        op(FileOp::Move {
            from: s("vendor"),
            to: s("v2")
        })
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    op(FileOp::Copy {
        from: s("vendor"),
        to: s("v3"),
    })
    .await
    .unwrap();
    assert!(repo_dir.join("v3/lib.rs").is_file() && gone("v3/.git"));

    // Chunked reads for downloads.
    let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    std::fs::write(repo_dir.join("blob.bin"), &data).unwrap();
    let c = f
        .mgr
        .read_file_chunk(&wt, "blob.bin", 9_000, 4_096)
        .await
        .unwrap();
    assert_eq!((c.size, c.data.as_slice()), (10_000, &data[9_000..]));
    let c = f
        .mgr
        .read_file_chunk(&wt, "blob.bin", 20_000, 10)
        .await
        .unwrap();
    assert!(c.data.is_empty());
    for bad in ["../secret", "src/escape", "src"] {
        assert!(matches!(
            f.mgr.read_file_chunk(&wt, bad, 0, 10).await,
            Err(SessionError::InvalidArgument(_))
        ));
    }
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

// ----- git status / diff ------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn git_status_and_diffs() {
    use civet_proto::GitChangeKind;
    let f = fixture();
    let repo_dir = make_repo(&f.root);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    let write = |p: &str, s: &[u8]| {
        let full = repo_dir.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, s).unwrap();
    };
    write("old.txt", b"one\ntwo\nthree\n");
    write("gone.txt", b"bye\n");
    write("img.bin", b"\x89PNG\0\0\x01");
    git(&repo_dir, &["add", "."]);
    git(&repo_dir, &["commit", "-q", "-m", "more"]);

    let clean = f.mgr.git_status(&wt).await.unwrap();
    assert!(clean.staged.is_empty() && clean.unstaged.is_empty() && clean.untracked.is_empty());

    write("README", b"hi\nthere\n");
    std::fs::remove_file(repo_dir.join("gone.txt")).unwrap();
    git(&repo_dir, &["mv", "old.txt", "new.txt"]);
    write("img.bin", b"\x89PNG\0\0\x02");
    write("dir/untracked file.txt", b"a\nb\nc");

    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(!s.truncated);
    assert_eq!(s.staged.len(), 1);
    assert_eq!(s.staged[0].path, "new.txt");
    assert_eq!(s.staged[0].old_path.as_deref(), Some("old.txt"));
    assert_eq!(s.staged[0].kind, GitChangeKind::Renamed);
    assert_eq!(
        (s.staged[0].additions, s.staged[0].deletions),
        (Some(0), Some(0))
    );
    let unstaged: Vec<_> = s
        .unstaged
        .iter()
        .map(|c| (c.path.as_str(), c.kind, c.additions, c.deletions))
        .collect();
    assert_eq!(
        unstaged,
        vec![
            ("README", GitChangeKind::Modified, Some(1), Some(0)),
            ("gone.txt", GitChangeKind::Deleted, Some(0), Some(1)),
            ("img.bin", GitChangeKind::Modified, None, None),
        ]
    );
    assert_eq!(s.untracked.len(), 1);
    assert_eq!(s.untracked[0].path, "dir/untracked file.txt");
    assert_eq!(s.untracked[0].additions, Some(3));

    let d = f.mgr.git_diff(&wt, "README", None, false, 3).await.unwrap();
    assert!(!d.binary && !d.too_large);
    assert!(
        d.diff.contains("@@ -1 +1,2 @@\n hi\n+there\n"),
        "{}",
        d.diff
    );

    let d = f
        .mgr
        .git_diff(&wt, "gone.txt", None, false, 3)
        .await
        .unwrap();
    assert!(d.diff.contains("deleted file mode") && d.diff.contains("-bye"));

    let d = f
        .mgr
        .git_diff(&wt, "new.txt", Some("old.txt"), true, 3)
        .await
        .unwrap();
    assert!(d.diff.contains("rename from old.txt"), "{}", d.diff);

    let d = f
        .mgr
        .git_diff(&wt, "img.bin", None, false, 3)
        .await
        .unwrap();
    assert!(d.binary && d.diff.is_empty());

    let d = f
        .mgr
        .git_diff(&wt, "dir/untracked file.txt", None, false, 3)
        .await
        .unwrap();
    assert_eq!((d.additions, d.deletions), (Some(3), Some(0)));
    assert!(
        d.diff.contains("new file mode") && d.diff.contains("+c"),
        "{}",
        d.diff
    );

    // Too many lines: flagged, no patch.
    let big: String = (0..30_000).map(|i| format!("{i}\n")).collect();
    write("big.txt", big.as_bytes());
    let d = f
        .mgr
        .git_diff(&wt, "big.txt", None, false, 3)
        .await
        .unwrap();
    assert!(d.too_large && d.diff.is_empty());
    assert_eq!(d.additions, Some(30_000));
    // Few lines but over the byte cap: git is cut off, still flagged.
    let wide = "x".repeat(4096) + "\n";
    write("wide.txt", wide.repeat(400).as_bytes());
    let d = f
        .mgr
        .git_diff(&wt, "wide.txt", None, false, 3)
        .await
        .unwrap();
    assert!(d.too_large && d.diff.is_empty());

    // Paths escaping the worktree are refused.
    for bad in ["../x", "/etc/passwd", ""] {
        assert!(matches!(
            f.mgr.git_diff(&wt, bad, None, false, 3).await,
            Err(SessionError::InvalidArgument(_))
        ));
    }
    std::os::unix::fs::symlink(&f.root, repo_dir.join("out")).unwrap();
    assert!(matches!(
        f.mgr.git_diff(&wt, "out/state/x", None, false, 3).await,
        Err(SessionError::InvalidArgument(_) | SessionError::NotFound(_))
    ));
    assert!(matches!(
        f.mgr
            .git_status(&WorktreeId::from("/no/such/worktree"))
            .await,
        Err(SessionError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn git_status_of_an_unborn_branch_and_a_conflict() {
    use civet_proto::GitChangeKind;
    let f = fixture();
    let repo_dir = f.root.join("fresh");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init", "-q", "-b", "main"]);
    std::fs::write(repo_dir.join("a.txt"), "x\n").unwrap();
    git(&repo_dir, &["add", "a.txt"]);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(s.staged.len(), 1);
    assert_eq!(s.staged[0].kind, GitChangeKind::Added);
    assert_eq!(s.staged[0].additions, Some(1));
    let d = f.mgr.git_diff(&wt, "a.txt", None, true, 3).await.unwrap();
    assert!(d.diff.contains("+x"), "{}", d.diff);

    git(&repo_dir, &["commit", "-q", "-m", "a"]);
    git(&repo_dir, &["checkout", "-q", "-b", "other"]);
    std::fs::write(repo_dir.join("a.txt"), "other\n").unwrap();
    git(&repo_dir, &["commit", "-q", "-am", "other"]);
    git(&repo_dir, &["checkout", "-q", "main"]);
    std::fs::write(repo_dir.join("a.txt"), "main\n").unwrap();
    git(&repo_dir, &["commit", "-q", "-am", "main"]);
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(["merge", "-q", "other"])
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(s.conflicted.len(), 1);
    assert_eq!(s.conflicted[0].path, "a.txt");
    assert!(s.staged.is_empty() && s.unstaged.is_empty());
    let d = f.mgr.git_diff(&wt, "a.txt", None, false, 3).await.unwrap();
    assert!(d.diff.contains("diff --cc"), "{}", d.diff);
}

// ----- git actions ------------------------------------------------------------

/// `git` with stdout, for assertions.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repo whose commits (made by the daemon's git, which has no identity
/// env) work: identity and no signing in its local config.
fn action_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    identity(&repo);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

fn identity(repo: &Path) {
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

async fn act(
    f: &Fixture,
    wt: &WorktreeId,
    action: civet_proto::GitAction,
) -> Result<Option<civet_proto::GitDiscardSnapshot>, SessionError> {
    f.mgr.git_action(wt, action).await
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn listed(v: &[civet_proto::GitFileChange]) -> Vec<&str> {
    v.iter().map(|c| c.path.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn git_stage_unstage_and_commit() {
    use civet_proto::GitAction;
    let f = fixture();
    let repo_dir = action_repo(&f.root, "proj");
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    std::fs::write(repo_dir.join("README"), "hi\nthere\n").unwrap();
    std::fs::write(repo_dir.join("a b.txt"), "a\n").unwrap();
    std::fs::write(repo_dir.join("*"), "glob\n").unwrap();

    // A literal "*" stages only itself.
    act(
        &f,
        &wt,
        GitAction::Stage {
            paths: paths(&["*"]),
        },
    )
    .await
    .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(listed(&s.staged), vec!["*"]);
    act(
        &f,
        &wt,
        GitAction::Stage {
            paths: paths(&["README", "a b.txt"]),
        },
    )
    .await
    .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(listed(&s.staged), vec!["*", "README", "a b.txt"]);
    act(
        &f,
        &wt,
        GitAction::Unstage {
            paths: paths(&["README", "*"]),
        },
    )
    .await
    .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(listed(&s.staged), vec!["a b.txt"]);
    assert_eq!(listed(&s.unstaged), vec!["README"]);
    assert_eq!(listed(&s.untracked), vec!["*"]);

    // Empty message refused; a real one commits only what is staged.
    let commit = |message: &str, amend, all| GitAction::Commit {
        message: message.into(),
        amend,
        all,
    };
    assert!(matches!(
        act(&f, &wt, commit("  ", false, false)).await,
        Err(SessionError::InvalidArgument(_))
    ));
    act(&f, &wt, commit("Add a b\n\nBody", false, false))
        .await
        .unwrap();
    assert_eq!(
        git_out(&repo_dir, &["log", "-1", "--format=%B"]),
        "Add a b\n\nBody"
    );
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(s.staged.is_empty());
    assert_eq!(listed(&s.unstaged), vec!["README"]);

    // Nothing staged: git's refusal comes back readable.
    match act(&f, &wt, commit("x", false, false)).await {
        Err(SessionError::Git(m)) => assert!(m.contains("no changes added to commit"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Commit all takes untracked files too; amend with no message keeps it.
    act(&f, &wt, commit("Everything", false, true))
        .await
        .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(s.unstaged.is_empty() && s.untracked.is_empty());
    std::fs::write(repo_dir.join("late.txt"), "late\n").unwrap();
    act(
        &f,
        &wt,
        GitAction::Stage {
            paths: paths(&["late.txt"]),
        },
    )
    .await
    .unwrap();
    act(&f, &wt, commit("", true, false)).await.unwrap();
    assert_eq!(
        git_out(&repo_dir, &["log", "-1", "--format=%s"]),
        "Everything"
    );
    assert_eq!(git_out(&repo_dir, &["rev-list", "--count", "HEAD"]), "3");
    assert!(git_out(&repo_dir, &["show", "--name-only", "--format=", "HEAD"]).contains("late.txt"));

    // Unstage on an unborn branch drops the index entry.
    let fresh = f.root.join("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    git(&fresh, &["init", "-q", "-b", "main"]);
    std::fs::write(fresh.join("x"), "x\n").unwrap();
    git(&fresh, &["add", "x"]);
    let r2 = f.mgr.add_repo(&fresh.to_string_lossy()).await.unwrap();
    let wt2 = f.mgr.list_worktrees(&r2.id).await.unwrap()[0].id.clone();
    act(
        &f,
        &wt2,
        GitAction::Unstage {
            paths: paths(&["x"]),
        },
    )
    .await
    .unwrap();
    let s = f.mgr.git_status(&wt2).await.unwrap();
    assert!(s.staged.is_empty());
    assert_eq!(listed(&s.untracked), vec!["x"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn git_discard_and_undo() {
    use civet_proto::GitAction;
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let repo_dir = action_repo(&f.root, "proj");
    let write = |p: &str, s: &[u8]| {
        let full = repo_dir.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, s).unwrap();
    };
    write("mod.txt", b"one\n");
    write("gone.txt", b"bye\n");
    write("run.sh", b"#!/bin/sh\n");
    std::fs::set_permissions(
        repo_dir.join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write("both.txt", b"base\n");
    git(&repo_dir, &["add", "."]);
    git(&repo_dir, &["commit", "-q", "-m", "files"]);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();

    write("mod.txt", b"one\ntwo\n");
    std::fs::remove_file(repo_dir.join("gone.txt")).unwrap();
    write("run.sh", b"#!/bin/sh\necho hi\n");
    write("new/deep/untracked.bin", b"\x00\x01\x02binary");
    std::os::unix::fs::symlink("mod.txt", repo_dir.join("link")).unwrap();
    // Staged and modified again.
    write("both.txt", b"staged\n");
    git(&repo_dir, &["add", "both.txt"]);
    write("both.txt", b"staged\nand more\n");
    // Staged new file.
    write("added.txt", b"added\n");
    git(&repo_dir, &["add", "added.txt"]);

    let before = f.mgr.git_status(&wt).await.unwrap();
    let all = paths(&[
        "mod.txt",
        "gone.txt",
        "run.sh",
        "new/deep/untracked.bin",
        "link",
        "both.txt",
        "added.txt",
    ]);

    // Unstaged discard: back to the index; untracked files deleted.
    let snap = act(
        &f,
        &wt,
        GitAction::Discard {
            paths: all.clone(),
            staged: false,
        },
    )
    .await
    .unwrap()
    .expect("snapshot");
    assert_eq!(std::fs::read(repo_dir.join("mod.txt")).unwrap(), b"one\n");
    assert_eq!(std::fs::read(repo_dir.join("gone.txt")).unwrap(), b"bye\n");
    assert_eq!(
        std::fs::read(repo_dir.join("both.txt")).unwrap(),
        b"staged\n"
    );
    assert!(!repo_dir.join("new").exists(), "empty dirs pruned");
    assert!(std::fs::symlink_metadata(repo_dir.join("link")).is_err());
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(s.unstaged.is_empty() && s.untracked.is_empty(), "{s:?}");
    assert_eq!(listed(&s.staged), vec!["added.txt", "both.txt"]);
    let untracked: Vec<_> = snap
        .entries
        .iter()
        .filter(|e| e.untracked)
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(untracked, vec!["new/deep/untracked.bin", "link"]);
    assert!(snap.entries.iter().all(|e| !e.restore_index));
    let gone = snap.entries.iter().find(|e| e.path == "gone.txt").unwrap();
    assert!(gone.file.is_none());

    // Undo puts every file back as it was (content, mode, symlink).
    act(&f, &wt, GitAction::RestoreDiscarded(snap))
        .await
        .unwrap();
    assert_eq!(f.mgr.git_status(&wt).await.unwrap(), before);
    assert_eq!(
        std::fs::read(repo_dir.join("new/deep/untracked.bin")).unwrap(),
        b"\x00\x01\x02binary"
    );
    assert_eq!(
        std::fs::read_link(repo_dir.join("link")).unwrap(),
        Path::new("mod.txt")
    );
    assert!(!repo_dir.join("gone.txt").exists());
    let mode = std::fs::metadata(repo_dir.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);

    // Staged discard: index and working tree back to HEAD; a staged new
    // file leaves both. Undo restores the index too.
    let snap = act(
        &f,
        &wt,
        GitAction::Discard {
            paths: all.clone(),
            staged: true,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(
        s.staged.is_empty() && s.unstaged.is_empty() && s.untracked.is_empty(),
        "{s:?}"
    );
    assert!(!repo_dir.join("added.txt").exists());
    assert_eq!(std::fs::read(repo_dir.join("both.txt")).unwrap(), b"base\n");
    act(&f, &wt, GitAction::RestoreDiscarded(snap))
        .await
        .unwrap();
    assert_eq!(f.mgr.git_status(&wt).await.unwrap(), before);
    assert_eq!(
        std::fs::read(repo_dir.join("both.txt")).unwrap(),
        b"staged\nand more\n"
    );
    assert_eq!(
        git_out(&repo_dir, &["show", ":both.txt"]),
        "staged",
        "index entry restored"
    );

    // Undo refuses once a discarded path changed again (an agent's newer
    // edit has no snapshot), and leaves that edit alone.
    for staged in [false, true] {
        let snap = act(
            &f,
            &wt,
            GitAction::Discard {
                paths: paths(&["mod.txt", "new/deep/untracked.bin"]),
                staged,
            },
        )
        .await
        .unwrap()
        .unwrap();
        write("mod.txt", b"agent wrote this\n");
        assert!(matches!(
            act(&f, &wt, GitAction::RestoreDiscarded(snap.clone())).await,
            Err(SessionError::InvalidArgument(_))
        ));
        assert_eq!(
            std::fs::read(repo_dir.join("mod.txt")).unwrap(),
            b"agent wrote this\n"
        );
        // Same for a deleted untracked file that came back.
        git(&repo_dir, &["checkout", "-q", "--", "mod.txt"]);
        write("new/deep/untracked.bin", b"recreated");
        assert!(matches!(
            act(&f, &wt, GitAction::RestoreDiscarded(snap)).await,
            Err(SessionError::InvalidArgument(_))
        ));
        assert_eq!(
            std::fs::read(repo_dir.join("new/deep/untracked.bin")).unwrap(),
            b"recreated"
        );
    }

    // A discard that fails part-way (a file that can't be deleted) rolls
    // back the paths it already discarded, so nothing is lost without Undo.
    let locked = repo_dir.join("locked");
    write("mod.txt", b"one\ntwo\n");
    write("locked/keep.txt", b"untracked\n");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = act(
        &f,
        &wt,
        GitAction::Discard {
            paths: paths(&["mod.txt", "added.txt", "locked/keep.txt"]),
            staged: true,
        },
    )
    .await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(r, Err(SessionError::Io(_))), "{r:?}");
    assert_eq!(
        std::fs::read(repo_dir.join("mod.txt")).unwrap(),
        b"one\ntwo\n"
    );
    assert_eq!(
        std::fs::read(repo_dir.join("added.txt")).unwrap(),
        b"added\n"
    );
    assert_eq!(
        git_out(&repo_dir, &["show", ":added.txt"]),
        "added",
        "index rolled back"
    );
    assert!(locked.join("keep.txt").exists());
    std::fs::remove_dir_all(&locked).unwrap();

    // Paths that leave the worktree or touch .git are refused, by every
    // path-taking action, and nothing is changed.
    std::os::unix::fs::symlink(&f.root, repo_dir.join("out")).unwrap();
    std::fs::write(f.root.join("victim"), "keep\n").unwrap();
    for bad in [
        "../x",
        "/etc/passwd",
        "",
        ".git/config",
        "sub/.git/HEAD",
        "out/victim",
    ] {
        let p = paths(&[bad]);
        for action in [
            GitAction::Stage { paths: p.clone() },
            GitAction::Unstage { paths: p.clone() },
            GitAction::Discard {
                paths: p.clone(),
                staged: true,
            },
        ] {
            let r = act(&f, &wt, action.clone()).await;
            assert!(
                matches!(r, Err(SessionError::InvalidArgument(_))),
                "{action:?}: {r:?}"
            );
        }
    }
    let restore = |path: &str, oid: &str| {
        GitAction::RestoreDiscarded(civet_proto::GitDiscardSnapshot {
            entries: vec![civet_proto::GitDiscardEntry {
                path: path.into(),
                file: Some(civet_proto::GitBlobRef {
                    oid: oid.into(),
                    mode: 0o100644,
                }),
                restore_index: false,
                index: None,
                untracked: true,
            }],
        })
    };
    let blob = git_out(&repo_dir, &["rev-parse", "HEAD:README"]);
    for bad in ["out/victim", "../victim", ".git/hooks/pre-commit"] {
        assert!(matches!(
            act(&f, &wt, restore(bad, &blob)).await,
            Err(SessionError::InvalidArgument(_))
        ));
    }
    assert!(matches!(
        act(&f, &wt, restore("ok.txt", "--output=/tmp/x")).await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert_eq!(std::fs::read(f.root.join("victim")).unwrap(), b"keep\n");
    assert!(!repo_dir.join(".git/hooks/pre-commit").exists());
}

/// main and `other` both change a.txt; `other` also deletes b.txt that
/// main changes. Merging `other` into main conflicts on both.
fn conflicted_repo(root: &Path) -> PathBuf {
    let repo = action_repo(root, "conflict");
    std::fs::write(repo.join("a.txt"), "base\n").unwrap();
    std::fs::write(repo.join("b.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "other"]);
    std::fs::write(repo.join("a.txt"), "theirs\n").unwrap();
    git(&repo, &["rm", "-q", "b.txt"]);
    git(&repo, &["commit", "-q", "-am", "other"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("a.txt"), "ours\n").unwrap();
    std::fs::write(repo.join("b.txt"), "ours\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "main"]);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["merge", "-q", "other"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    repo
}

#[tokio::test(flavor = "multi_thread")]
async fn git_accept_conflicts() {
    use civet_proto::{ConflictSide, GitAction};
    let f = fixture();
    let repo_dir = conflicted_repo(&f.root);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    assert_eq!(
        listed(&f.mgr.git_status(&wt).await.unwrap().conflicted),
        vec!["a.txt", "b.txt"]
    );
    // Conflicts can't be discarded, nor a clean path "accepted".
    assert!(matches!(
        act(
            &f,
            &wt,
            GitAction::Discard {
                paths: paths(&["a.txt"]),
                staged: false
            }
        )
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert!(matches!(
        act(
            &f,
            &wt,
            GitAction::AcceptConflict {
                paths: paths(&["README"]),
                side: ConflictSide::Ours
            }
        )
        .await,
        Err(SessionError::InvalidArgument(_))
    ));

    act(
        &f,
        &wt,
        GitAction::AcceptConflict {
            paths: paths(&["a.txt", "b.txt"]),
            side: ConflictSide::Theirs,
        },
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(repo_dir.join("a.txt")).unwrap(), b"theirs\n");
    // Theirs deleted b.txt: taking their side removes it.
    assert!(!repo_dir.join("b.txt").exists());
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(s.conflicted.is_empty(), "{s:?}");
    assert_eq!(listed(&s.staged), vec!["a.txt", "b.txt"]);

    // Ours, and "mark resolved" (stage) of a hand-edited file.
    git(&repo_dir, &["merge", "--abort"]);
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(["merge", "-q", "other"])
        .output()
        .unwrap();
    act(
        &f,
        &wt,
        GitAction::AcceptConflict {
            paths: paths(&["b.txt"]),
            side: ConflictSide::Ours,
        },
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(repo_dir.join("b.txt")).unwrap(), b"ours\n");
    std::fs::write(repo_dir.join("a.txt"), "merged by hand\n").unwrap();
    act(
        &f,
        &wt,
        GitAction::Stage {
            paths: paths(&["a.txt"]),
        },
    )
    .await
    .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert!(s.conflicted.is_empty());
    act(
        &f,
        &wt,
        GitAction::Commit {
            message: "Merge other".into(),
            amend: false,
            all: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        git_out(&repo_dir, &["rev-list", "--count", "--merges", "HEAD"]),
        "1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn git_push_pull_fetch_and_undo_commit() {
    use civet_proto::GitAction;
    let f = fixture();
    let origin = f.root.join("origin.git");
    git(
        &f.root,
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    let repo_dir = action_repo(&f.root, "proj");
    git(
        &repo_dir,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo_dir, &["push", "-q", "-u", "origin", "main"]);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();

    // An agent worktree: a new branch, upstream configured but not pushed.
    let w = f
        .mgr
        .create_worktree(&repo.id, "feat/x", Some("main"), None)
        .await
        .unwrap();
    let wt = w.id.clone();
    let wdir = PathBuf::from(&w.path);
    let b = f.mgr.git_branch(&wt).await.unwrap();
    assert_eq!(b.branch.as_deref(), Some("feat/x"));
    assert_eq!(b.upstream, None);
    assert!(b.has_remote);
    assert!(!b.can_undo_commit, "HEAD is main's pushed commit");

    std::fs::write(wdir.join("f.txt"), "f\n").unwrap();
    act(
        &f,
        &wt,
        GitAction::Commit {
            message: "f".into(),
            amend: false,
            all: true,
        },
    )
    .await
    .unwrap();
    // Undo needs the current HEAD; a stale one is refused.
    let b = f.mgr.git_branch(&wt).await.unwrap();
    assert!(b.can_undo_commit);
    assert_eq!(b.ahead, 0, "no upstream yet");
    assert!(matches!(
        act(
            &f,
            &wt,
            GitAction::UndoCommit {
                head: "0".repeat(40)
            }
        )
        .await,
        Err(SessionError::InvalidArgument(_))
    ));
    let head = b.head.clone().unwrap();
    act(&f, &wt, GitAction::UndoCommit { head: head.clone() })
        .await
        .unwrap();
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(
        listed(&s.staged),
        vec!["f.txt"],
        "soft reset keeps the index"
    );
    act(
        &f,
        &wt,
        GitAction::Commit {
            message: "f again".into(),
            amend: false,
            all: false,
        },
    )
    .await
    .unwrap();

    // First push creates the remote branch and tracks it.
    act(&f, &wt, GitAction::Push).await.unwrap();
    let b = f.mgr.git_branch(&wt).await.unwrap();
    assert_eq!(b.upstream.as_deref(), Some("origin/feat/x"));
    assert_eq!((b.ahead, b.behind), (0, 0));
    assert!(!b.can_undo_commit, "HEAD is pushed");
    let head = b.head.clone().unwrap();
    assert!(matches!(
        act(&f, &wt, GitAction::UndoCommit { head }).await,
        Err(SessionError::InvalidArgument(_))
    ));
    assert_eq!(
        git_out(&origin, &["rev-parse", "refs/heads/feat/x"]),
        git_out(&wdir, &["rev-parse", "HEAD"])
    );

    // Someone else pushes; fetch sees it, pull merges it.
    let other = f.root.join("other");
    git(
        &f.root,
        &[
            "clone",
            "-q",
            "-b",
            "feat/x",
            &origin.to_string_lossy(),
            "other",
        ],
    );
    identity(&other);
    std::fs::write(other.join("theirs.txt"), "t\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q"]);
    std::fs::write(wdir.join("mine.txt"), "m\n").unwrap();
    act(
        &f,
        &wt,
        GitAction::Commit {
            message: "mine".into(),
            amend: false,
            all: true,
        },
    )
    .await
    .unwrap();
    act(&f, &wt, GitAction::Fetch).await.unwrap();
    let b = f.mgr.git_branch(&wt).await.unwrap();
    assert_eq!((b.ahead, b.behind), (1, 1));
    // Diverged: push is rejected with git's message.
    match act(&f, &wt, GitAction::Push).await {
        Err(SessionError::Git(m)) => assert!(m.contains("rejected"), "{m}"),
        other => panic!("{other:?}"),
    }
    act(&f, &wt, GitAction::Pull).await.unwrap();
    let b = f.mgr.git_branch(&wt).await.unwrap();
    assert_eq!((b.ahead, b.behind), (2, 0));
    assert!(wdir.join("theirs.txt").exists());
    act(&f, &wt, GitAction::Push).await.unwrap();

    // A pull that conflicts reports it and leaves the conflict to resolve.
    std::fs::write(other.join("theirs.txt"), "other side\n").unwrap();
    git(&other, &["pull", "-q", "--no-rebase"]);
    git(&other, &["commit", "-q", "-am", "edit"]);
    git(&other, &["push", "-q"]);
    std::fs::write(wdir.join("theirs.txt"), "my side\n").unwrap();
    act(
        &f,
        &wt,
        GitAction::Commit {
            message: "edit mine".into(),
            amend: false,
            all: true,
        },
    )
    .await
    .unwrap();
    match act(&f, &wt, GitAction::Pull).await {
        Err(SessionError::Git(m)) => assert!(m.contains("CONFLICT"), "{m}"),
        other => panic!("{other:?}"),
    }
    let s = f.mgr.git_status(&wt).await.unwrap();
    assert_eq!(listed(&s.conflicted), vec!["theirs.txt"]);

    // No remote at all: push says so.
    let lone = action_repo(&f.root, "lone");
    let r2 = f.mgr.add_repo(&lone.to_string_lossy()).await.unwrap();
    let wt2 = f.mgr.list_worktrees(&r2.id).await.unwrap()[0].id.clone();
    assert!(!f.mgr.git_branch(&wt2).await.unwrap().has_remote);
    match act(&f, &wt2, GitAction::Push).await {
        Err(SessionError::Git(m)) => assert!(m.contains("no remote"), "{m}"),
        other => panic!("{other:?}"),
    }
    match act(&f, &wt2, GitAction::Pull).await {
        Err(SessionError::Git(m)) => assert!(m.contains("no upstream"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn git_stash_is_per_branch() {
    use civet_proto::GitAction;
    let f = fixture();
    let repo_dir = action_repo(&f.root, "proj");
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let main = f.mgr.list_worktrees(&repo.id).await.unwrap()[0].id.clone();
    let w = f
        .mgr
        .create_worktree(&repo.id, "side", Some("main"), None)
        .await
        .unwrap();
    let side = PathBuf::from(&w.path);

    assert!(matches!(
        act(&f, &main, GitAction::StashAll).await,
        Err(SessionError::Git(_))
    ));
    std::fs::write(repo_dir.join("README"), "main edit\n").unwrap();
    std::fs::write(repo_dir.join("new.txt"), "untracked\n").unwrap();
    act(&f, &main, GitAction::StashAll).await.unwrap();
    let s = f.mgr.git_status(&main).await.unwrap();
    assert!(s.unstaged.is_empty() && s.untracked.is_empty());
    assert_eq!(f.mgr.git_branch(&main).await.unwrap().stashes, 1);

    // The other worktree's stash (newer, same shared list) is not ours.
    std::fs::write(side.join("README"), "side edit\n").unwrap();
    act(&f, &w.id, GitAction::StashAll).await.unwrap();
    assert_eq!(f.mgr.git_branch(&w.id).await.unwrap().stashes, 1);
    assert_eq!(f.mgr.git_branch(&main).await.unwrap().stashes, 1);

    act(&f, &main, GitAction::StashPop).await.unwrap();
    assert_eq!(
        std::fs::read(repo_dir.join("README")).unwrap(),
        b"main edit\n"
    );
    assert!(repo_dir.join("new.txt").exists());
    assert_eq!(f.mgr.git_branch(&main).await.unwrap().stashes, 0);
    assert!(matches!(
        act(&f, &main, GitAction::StashPop).await,
        Err(SessionError::Git(_))
    ));
    // The side entry is still there and still pops into its worktree.
    assert_eq!(git_out(&repo_dir, &["stash", "list"]).lines().count(), 1);
    act(&f, &w.id, GitAction::StashPop).await.unwrap();
    assert_eq!(std::fs::read(side.join("README")).unwrap(), b"side edit\n");

    // Both worktrees stash and pop at the same time: each gets its own back
    // and the shared list ends empty.
    std::fs::write(repo_dir.join("README"), "main again\n").unwrap();
    std::fs::write(side.join("README"), "side again\n").unwrap();
    let (a, b) = tokio::join!(
        act(&f, &main, GitAction::StashAll),
        act(&f, &w.id, GitAction::StashAll)
    );
    a.unwrap();
    b.unwrap();
    let (a, b) = tokio::join!(
        act(&f, &main, GitAction::StashPop),
        act(&f, &w.id, GitAction::StashPop)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        std::fs::read(repo_dir.join("README")).unwrap(),
        b"main again\n"
    );
    assert_eq!(std::fs::read(side.join("README")).unwrap(), b"side again\n");
    assert_eq!(git_out(&repo_dir, &["stash", "list"]), "");
}
