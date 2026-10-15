//! The SSH bootstrap path (probe → upload → start → resolve, then QUIC and
//! the stdio-tunnel fallback) driven through `civet-cli`, with a fake `ssh`
//! that runs the remote command locally under a temporary `$HOME`. Nothing
//! touches the real `~/.civet` or `~/.ssh`.

#![cfg(feature = "server")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");
const CLI: &str = env!("CARGO_BIN_EXE_civet-cli");

/// Behaves like `ssh [opts] -- DEST CMD` / `ssh -G … -- DEST` for the
/// options civet passes, running CMD with `sh -c` (as sshd would with a
/// POSIX login shell) under $FAKE_REMOTE_HOME.
const FAKE_SSH: &str = r#"#!/bin/sh
if [ "$1" = "-G" ]; then
  printf 'host fake\nhostname 127.0.0.1\nport 22\n'
  exit 0
fi
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-J|-i) shift 2 ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
dest="$1"; shift
[ "$dest" = "fakehost" ] || { echo "unknown host $dest" >&2; exit 255; }
unset CIVET_STATE_DIR
HOME="$FAKE_REMOTE_HOME"; export HOME
cd "$HOME" || exit 255
exec sh -c "$*"
"#;

impl Drop for Env {
    /// Never leave the "remote" daemon running, even when an assert failed.
    fn drop(&mut self) {
        if let Some(pid) = remote_daemon_pid(&self.home) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
}

fn uname(flag: &str) -> String {
    let out = Command::new("uname").arg(flag).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

struct Env {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    client_state: std::path::PathBuf,
    dist: std::path::PathBuf,
    ssh: std::path::PathBuf,
}

fn setup() -> Env {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("remote-home");
    let client_state = tmp.path().join("client-state");
    let dist = tmp.path().join("dist");
    for d in [&home, &client_state, &dist] {
        std::fs::create_dir_all(d).unwrap();
    }
    let triple = civetd::bootstrap::target_triple(&uname("-s"), &uname("-m")).unwrap();
    std::fs::copy(CIVETD, dist.join(format!("civetd-{triple}"))).unwrap();
    let ssh = tmp.path().join("fake-ssh");
    std::fs::write(&ssh, FAKE_SSH).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    Env {
        home,
        client_state,
        dist,
        ssh,
        _tmp: tmp,
    }
}

fn cli(env: &Env, args: &[&str]) -> std::process::Output {
    Command::new(CLI)
        .env("CIVET_STATE_DIR", &env.client_state)
        .env("FAKE_REMOTE_HOME", &env.home)
        .env_remove("CIVET_DIST_DIR")
        .arg("--ssh")
        .arg("fakehost")
        .arg("--ssh-program")
        .arg(&env.ssh)
        .arg("--dist")
        .arg(&env.dist)
        .arg("-v")
        .args(args)
        .output()
        .unwrap()
}

fn remote_daemon_pid(home: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(home.join(".civet/daemon.json")).ok()?;
    Some(civet_proto::BootstrapInfo::from_json_line(&s).ok()?.pid)
}

#[test]
fn ssh_bootstrap_install_reattach_and_tunnel() {
    let env = setup();

    // 1. Fresh remote: probe finds no binary → upload → start → QUIC.
    let out = cli(&env, &["ping", "--count", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("installed: true"), "stderr: {stderr}");
    assert!(stderr.contains("reattached: false"), "stderr: {stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("min/avg/max"));
    let bin = env
        .home
        .join(format!(".civet/bin/civetd-{}", civetd::VERSION));
    assert!(bin.is_file(), "binary installed version-suffixed");
    assert!(env.home.join(".civet/bin/civetd").exists(), "symlink");
    let pid = remote_daemon_pid(&env.home).expect("remote daemon running");

    // 2. Binary present: one round trip, reattach to the same daemon.
    let out = cli(&env, &["panes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("installed: false"), "stderr: {stderr}");
    assert!(stderr.contains("reattached: true"), "stderr: {stderr}");
    assert_eq!(remote_daemon_pid(&env.home), Some(pid));

    // 3. UDP-blocked fallback: same protocol through `ssh … stdio-tunnel`.
    let out = cli(&env, &["--fallback", "info"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info["pid"].as_u64(), Some(u64::from(pid)));

    // 4. Non-interactive attach over ssh+QUIC.
    let mut child = Command::new(CLI)
        .env("CIVET_STATE_DIR", &env.client_state)
        .env("FAKE_REMOTE_HOME", &env.home)
        .args(["--ssh", "fakehost", "--ssh-program"])
        .arg(&env.ssh)
        .arg("--dist")
        .arg(&env.dist)
        .args(["connect", "--", "sh"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"echo over-ssh-$((6*7))\nexit\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let screen = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(screen.contains("over-ssh-42"), "screen: {screen:?}");

    // Stop the "remote" daemon.
    let _ = Command::new("kill").arg(pid.to_string()).status();
    let deadline = Instant::now() + Duration::from_secs(10);
    while remote_daemon_pid(&env.home).is_some() {
        assert!(Instant::now() < deadline, "remote daemon did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The client side used only its own state dir for ControlPath sockets.
    assert!(env.client_state.join("ssh").is_dir());
}
