//! `civetd handoff` (the real binary) against an in-process daemon in a
//! temp state dir: worktree + pane created, branch collisions suffixed,
//! uncommitted changes carried, clear errors outside Civet / without a
//! daemon.

#![cfg(feature = "server")]

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use civetd::daemon::{Daemon, DaemonOptions};

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn make_repo(root: &Path) -> PathBuf {
    let repo = root.join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    // Canonical, as the daemon reports paths (macOS /var -> /private/var).
    std::fs::canonicalize(repo).unwrap()
}

async fn handoff(state: &Path, in_pane: bool, args: &[&str]) -> Output {
    let mut cmd = tokio::process::Command::new(CIVETD);
    cmd.arg("handoff")
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env_remove("CIVET_STATE_DIR");
    if in_pane {
        cmd.env("CIVET_PANE_ID", "1");
    } else {
        cmd.env_remove("CIVET_PANE_ID");
    }
    cmd.output().await.unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "handoff failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

async fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "{} never written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_creates_worktree_and_pane() {
    let state = tempfile::tempdir().unwrap();
    let mut opts = DaemonOptions::new(state.path());
    opts.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    opts.port_range = 0..=0;
    opts.shell = Some("/bin/sh".into());
    let daemon = Daemon::start(opts).await.unwrap();

    let root = tempfile::tempdir().unwrap();
    let repo = make_repo(root.path());
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    // Uncommitted work: a modified file and an untracked one.
    std::fs::write(repo.join("README"), "hi\nchanged\n").unwrap();
    std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();

    // A fake agent: writes the prompt (its last argument) to a file.
    let fake = [
        "--",
        "/bin/sh",
        "-c",
        "printf '%s' \"$1\" > prompt.txt; sleep 30",
        "fake-agent",
    ];
    let prompt = "# Fix the flaky test\n\nIt fails with \"quoted\" $HOME text.";
    let repo_s = repo.display().to_string();
    let mut args = vec!["--prompt", prompt, "--from", &repo_s, "--carry-changes"];
    args.extend(fake);
    let v = json(&handoff(state.path(), true, &args).await);
    assert_eq!(v["branch"], "fix-the-flaky-test");
    assert_eq!(v["title"], "Fix the flaky test");
    assert_eq!(v["base"], head.as_str());
    assert_eq!(v["agent"], "claude");
    assert_eq!(v["carriedChanges"], true);
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    assert_eq!(
        wt,
        repo.parent()
            .unwrap()
            .join("proj-worktrees/fix-the-flaky-test")
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("README")).unwrap(),
        "hi\nchanged\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("new.txt")).unwrap(),
        "untracked\n"
    );
    // The source checkout is untouched.
    assert!(git(&repo, &["status", "--porcelain"]).contains("?? new.txt"));
    assert_eq!(wait_for_file(&wt.join("prompt.txt")).await, prompt);

    // The daemon knows the repo, worktree and pane (what the app shows).
    let pane = civet_proto::PaneId(v["pane"].as_u64().unwrap());
    let info = daemon.sessions().pane_info(pane).unwrap();
    assert_eq!(info.title, "Fix the flaky test");
    assert_eq!(info.worktree.unwrap().0, wt.display().to_string());
    assert_eq!(daemon.sessions().list_repos().await.unwrap().len(), 1);

    // Same title again: the branch gets a suffix; the repo is reused.
    let mut args = vec!["--prompt", prompt, "--from", &repo_s, "--agent", "codex"];
    args.extend(fake);
    let v = json(&handoff(state.path(), true, &args).await);
    assert_eq!(v["branch"], "fix-the-flaky-test-2");
    assert_eq!(v["agent"], "codex");
    assert_eq!(v["carriedChanges"], false);
    assert_eq!(daemon.sessions().list_repos().await.unwrap().len(), 1);

    // An explicit branch that exists is an error, not a suffix.
    let mut args = vec![
        "--prompt",
        "x",
        "--from",
        &repo_s,
        "--branch",
        "fix-the-flaky-test",
    ];
    args.extend(fake);
    let out = handoff(state.path(), true, &args).await;
    assert!(!out.status.success());

    // Outside a Civet pane.
    let out = handoff(state.path(), false, &["--prompt", "x", "--from", &repo_s]).await;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not running inside a Civet pane"), "{err}");

    daemon.shutdown("test").await;

    // No daemon.
    let empty = tempfile::tempdir().unwrap();
    let out = handoff(empty.path(), true, &["--prompt", "x", "--from", &repo_s]).await;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no Civet daemon is reachable"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_applies_the_worktree_template() {
    let state = tempfile::tempdir().unwrap();
    let mut opts = DaemonOptions::new(state.path());
    opts.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    opts.port_range = 0..=0;
    opts.shell = Some("/bin/sh".into());
    let daemon = Daemon::start(opts).await.unwrap();

    let root = tempfile::tempdir().unwrap();
    let repo = make_repo(root.path());
    let repo_s = repo.display().to_string();
    // The template's agent: a script that records its argv and env.
    let agent = root.path().join("my-agent");
    std::fs::write(
        &agent,
        "#!/bin/sh\nprintf '%s|' \"$@\" > args.txt\necho \"$CIVET_PORT\" > port.txt\nsleep 30\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(repo.join(".civet")).unwrap();
    std::fs::write(
        repo.join(".civet/worktree.toml"),
        format!(
            "setup = \"echo set-up > setup.txt\"\ncopy = [\".env\"]\nagent = \"'{}' --flag\"\nprompt = \"Conventions first.\"\n",
            agent.display()
        ),
    )
    .unwrap();
    std::fs::write(repo.join(".env"), "K=V\n").unwrap();

    let v = json(
        &handoff(
            state.path(),
            true,
            &["--prompt", "Do the thing", "--from", &repo_s],
        )
        .await,
    );
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    assert_eq!(v["agent"], "my-agent");
    assert_eq!(v["copied"], serde_json::json!([".env"]));
    assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "K=V\n");
    assert!(v["setupPane"].is_u64(), "{v}");
    assert_eq!(wait_for_file(&wt.join("setup.txt")).await, "set-up\n");
    assert_eq!(
        wait_for_file(&wt.join("args.txt")).await,
        "--flag|Conventions first.\n\nDo the thing|"
    );
    let port = v["port"].as_u64().unwrap();
    assert_eq!(
        wait_for_file(&wt.join("port.txt")).await.trim(),
        port.to_string()
    );
    let setup = daemon
        .sessions()
        .pane_info(civet_proto::PaneId(v["setupPane"].as_u64().unwrap()))
        .unwrap();
    assert_eq!(setup.title, "Setup");

    // --agent overrides the template's command; --no-setup skips it all.
    let fake = [
        "--",
        "/bin/sh",
        "-c",
        "printf '%s' \"$1\" > p.txt; sleep 30",
        "x",
    ];
    let mut args = vec!["--prompt", "Other", "--from", &repo_s, "--no-setup"];
    args.extend(fake);
    let v = json(&handoff(state.path(), true, &args).await);
    let wt = PathBuf::from(v["worktree"].as_str().unwrap());
    assert_eq!(v["setupPane"], serde_json::Value::Null);
    assert_eq!(v["copied"], serde_json::json!([]));
    assert!(!wt.join(".env").exists());
    assert_eq!(wait_for_file(&wt.join("p.txt")).await, "Other");

    daemon.shutdown("test").await;
}
