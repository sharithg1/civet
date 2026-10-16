//! Integration tests for the agent hook shims (real PTYs, real shells) and
//! the git config / option-injection rules of `create_worktree`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use civet_proto::{
    AgentStatus, AgentStatusSource, ClientId, CommandSpec, Event, InputMessage, PaneId,
    SpawnPaneRequest, TermSize,
};
use civet_session::{SessionConfig, SessionError, SessionManager};
use tokio::sync::broadcast;

const T: Duration = Duration::from_secs(15);

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    mgr: SessionManager,
}

fn fixture() -> Fixture {
    fixture_shell("/bin/sh")
}

fn fixture_shell(shell: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut cfg = SessionConfig::new(root.join("state"));
    cfg.shell = Some(shell.into());
    cfg.idle_after = Duration::from_millis(500);
    let mgr = SessionManager::new(cfg).unwrap();
    Fixture {
        _dir: dir,
        root,
        mgr,
    }
}

fn git_out(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn make_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// Keys of the repository's shared config file (`.git/config`).
fn shared_config_keys(repo: &Path) -> Vec<String> {
    let out = git_out(
        repo,
        &["config", "--file", ".git/config", "--list", "--name-only"],
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_config_is_per_branch_only() {
    let f = fixture();
    // No origin: nothing is written.
    let repo_dir = make_repo(&f.root, "solo");
    let before = shared_config_keys(&repo_dir);
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    f.mgr
        .create_worktree(&repo.id, "feat/a", None, None)
        .await
        .unwrap();
    f.mgr
        .create_worktree(&repo.id, "feat/b", Some("main"), None)
        .await
        .unwrap();
    let after = shared_config_keys(&repo_dir);
    assert!(
        after.iter().all(|k| !k.starts_with("push.")),
        "push.* in shared config: {after:?}"
    );
    assert_eq!(before, after, "config changed without an origin remote");

    // With origin: only branch.<b>.remote/merge for the new branch.
    let upstream = make_repo(&f.root, "upstream");
    let repo_dir = f.root.join("clone");
    git(
        &f.root,
        &["clone", "-q", &upstream.to_string_lossy(), "clone"],
    );
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let wt = f
        .mgr
        .create_worktree(&repo.id, "agent/x", Some("origin/main"), None)
        .await
        .unwrap();
    let keys = shared_config_keys(&repo_dir);
    assert!(keys.iter().all(|k| !k.starts_with("push.")), "{keys:?}");
    assert_eq!(
        git(&repo_dir, &["config", "branch.agent/x.remote"]).trim(),
        "origin"
    );
    assert_eq!(
        git(&repo_dir, &["config", "branch.agent/x.merge"]).trim(),
        "refs/heads/agent/x"
    );
    // The worktree itself has no worktree-specific push config either.
    let wt_keys = git_out(
        Path::new(&wt.path),
        &["config", "--worktree", "--list", "--name-only"],
    );
    assert!(!String::from_utf8_lossy(&wt_keys.stdout).contains("push."));
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_base_cannot_inject_options() {
    let f = fixture();
    let repo_dir = make_repo(&f.root, "proj");
    let repo = f.mgr.add_repo(&repo_dir.to_string_lossy()).await.unwrap();
    let marker = f.root.join("pwned");
    for base in [
        format!("--output={}", marker.display()),
        "-h".to_owned(),
        "--detach".to_owned(),
        "nope-not-a-ref".to_owned(),
        "main\n--force".to_owned(),
    ] {
        let r = f
            .mgr
            .create_worktree(&repo.id, "inj", Some(&base), None)
            .await;
        assert!(
            matches!(r, Err(SessionError::InvalidArgument(_))),
            "{base:?}: {r:?}"
        );
    }
    assert!(!marker.exists());
    // A real rev still works (tag / sha).
    let sha = git(&repo_dir, &["rev-parse", "HEAD"]);
    f.mgr
        .create_worktree(&repo.id, "ok", Some(sha.trim()), None)
        .await
        .unwrap();
}

async fn next_status(
    rx: &mut broadcast::Receiver<Event>,
    pane: PaneId,
) -> civet_proto::AgentStatusEvent {
    let fut = async {
        loop {
            match rx.recv().await {
                Ok(Event::AgentStatus(s)) if s.pane == pane => return s,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("event channel closed"),
            }
        }
    };
    tokio::time::timeout(T, fut)
        .await
        .expect("timed out waiting for a status event")
}

/// Wait for `status` from a hook, skipping other events.
async fn wait_hook_status(
    rx: &mut broadcast::Receiver<Event>,
    pane: PaneId,
    status: AgentStatus,
) -> civet_proto::AgentStatusEvent {
    loop {
        let s = next_status(rx, pane).await;
        if s.status == status {
            assert_eq!(s.source, AgentStatusSource::Hook, "{s:?}");
            return s;
        }
    }
}

/// A fake `claude` that behaves like Claude Code with hooks: it requires
/// `--settings <file>` (added by the PATH shim), takes the hook script from
/// that file and fires UserPromptSubmit, a PreToolUse, a permission
/// Notification, a parallel subagent's PreToolUse (must not clear the
/// prompt), PostToolUse and Stop. Each transition waits for the test to
/// acknowledge the previous one, so polling cannot miss a short-lived status.
fn fake_claude(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    let body = r#"#!/bin/sh
if [ "$1" != "--settings" ]; then echo "fake-claude: no settings: $*"; sleep 30; exit 1; fi
settings="$2"
hook=$(sed -n "s/.*\"command\": \"'\(.*\)' done\".*/\1/p" "$settings" | head -n 1)
echo "fake-claude hook=$hook"
advance() { IFS= read -r step && [ "$step" = "$1" ] || exit 1; }
printf '{"hook_event_name":"UserPromptSubmit","prompt":"hi"}' | "$hook" running
advance permission
printf '{"hook_event_name":"PreToolUse","tool_name":"Bash"}' | "$hook" running soft
printf '{"hook_event_name":"Notification","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}' | "$hook" needs_input
advance soft
printf '{"hook_event_name":"PreToolUse","tool_name":"Read"}' | "$hook" running soft
echo fake-claude-soft-fired
advance resume
printf '{"hook_event_name":"PostToolUse","tool_name":"Bash"}' | "$hook" running
advance stop
printf '{"hook_event_name":"Stop"}' | "$hook" done
echo fake-claude-finished
sleep 30
"#;
    let p = bin.join("claude");
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn wait_output(f: &Fixture, pane: PaneId, marker: &str) {
    tokio::time::timeout(T, async {
        loop {
            if f.mgr
                .screen_text(pane)
                .unwrap()
                .iter()
                .any(|line| line.contains(marker))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {marker:?}"));
}

async fn run_claude_flow(f: &Fixture, pane: PaneId, rx: &mut broadcast::Receiver<Event>) {
    let s = wait_hook_status(rx, pane, AgentStatus::Running).await;
    assert_eq!(s.source, AgentStatusSource::Hook);
    // The interactive-shell caller already used input sequence numbers 1 and 2.
    input(f, pane, 10, "permission\n");
    let s = wait_hook_status(rx, pane, AgentStatus::NeedsInput).await;
    assert_eq!(
        s.message.as_deref(),
        Some("Claude needs your permission to use Bash")
    );
    // The subagent's PreToolUse ("soft") must not end the prompt.
    input(f, pane, 11, "soft\n");
    wait_output(f, pane, "fake-claude-soft-fired").await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), next_status(rx, pane))
            .await
            .is_err(),
        "soft PreToolUse must not change the pending permission status"
    );
    assert_eq!(
        f.mgr.pane_info(pane).unwrap().status,
        AgentStatus::NeedsInput
    );
    input(f, pane, 12, "resume\n");
    let s = next_status(rx, pane).await;
    assert_eq!(s.status, AgentStatus::Running, "{s:?}");
    assert_eq!(s.source, AgentStatusSource::Hook);
    input(f, pane, 13, "stop\n");
    wait_hook_status(rx, pane, AgentStatus::Done).await;
    wait_output(f, pane, "fake-claude-finished").await;
}

/// Environment that hides any real `claude` of the machine running the
/// tests (never start the real one) and the user's own rc files.
fn isolated_env(f: &Fixture, bin: &Path) -> Vec<(String, String)> {
    let home = f.root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
        ("PS1".into(), "$ ".into()),
    ]
}

fn input(f: &Fixture, pane: PaneId, seq: u64, line: &str) {
    f.mgr
        .write_input(
            ClientId([3; 16]),
            &InputMessage {
                pane,
                seq,
                data: line.as_bytes().to_vec(),
            },
        )
        .unwrap();
}

/// `claude` typed in a bash shell pane goes through the shim (even after the
/// user reset PATH: the PROMPT_COMMAND integration puts the shim dir back
/// first) and the pane gets agent status.
#[tokio::test(flavor = "multi_thread")]
async fn claude_typed_in_shell_pane_reports_status() {
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let f = fixture_shell("/bin/bash");
    let bin = f.root.join("realbin");
    fake_claude(&bin);
    let mut rx = f.mgr.subscribe_events();
    let info = f
        .mgr
        .spawn_pane(SpawnPaneRequest {
            worktree: None,
            cwd: Some(f.root.to_string_lossy().into_owned()),
            command: None,
            env: isolated_env(&f, &bin),
            size: TermSize::new(120, 30),
            title: None,
            agent: None,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    input(
        &f,
        info.id,
        1,
        &format!("export PATH=\"{}:/usr/bin:/bin\"\n", bin.display()),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    input(&f, info.id, 2, "cd / && FOO=1 claude\n");
    run_claude_flow(&f, info.id, &mut rx).await;
    let screen = f.mgr.screen_text(info.id).unwrap().join("\n");
    assert!(screen.contains("fake-claude hook=/"), "{screen}");
    assert!(screen.contains("fake-claude-finished"), "{screen}");
}

/// A custom command string (`cd x && claude`) launched as a pane.
#[tokio::test(flavor = "multi_thread")]
async fn claude_in_command_string_reports_status() {
    let f = fixture();
    let bin = f.root.join("realbin");
    fake_claude(&bin);
    let mut rx = f.mgr.subscribe_events();
    let info = f
        .mgr
        .spawn_pane(SpawnPaneRequest {
            worktree: None,
            cwd: None,
            command: Some(CommandSpec::new("cd / && claude", Vec::<String>::new())),
            env: isolated_env(&f, &bin),
            size: TermSize::new(120, 30),
            title: None,
            agent: None,
        })
        .await
        .unwrap();
    // The displayed command is the user's string, unmodified.
    assert!(!info.command.join(" ").contains("--settings"));
    run_claude_flow(&f, info.id, &mut rx).await;
}

/// Pane environment: shim dir first, TERM_PROGRAM=civet.
#[tokio::test(flavor = "multi_thread")]
async fn pane_env_has_shim_first() {
    let f = fixture();
    let info = f
        .mgr
        .spawn_pane(SpawnPaneRequest {
            worktree: None,
            cwd: None,
            command: Some(CommandSpec::new(
                "sh",
                [
                    "-c",
                    "echo \"P=${PATH%%:*}|$TERM_PROGRAM|$CIVET_SHIM_DIR\"; sleep 30",
                ],
            )),
            env: vec![],
            size: TermSize::new(200, 10),
            title: None,
            agent: None,
        })
        .await
        .unwrap();
    let shim = f.root.join("state/hooks/bin");
    let want = format!("P={}|civet|{}", shim.display(), shim.display());
    let start = Instant::now();
    loop {
        let text = f.mgr.screen_text(info.id).unwrap().join("\n");
        if text.contains(&want) {
            break;
        }
        assert!(start.elapsed() < T, "{want} not in {text}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
