//! Bootstrap over a fake `ssh` (runs the remote command locally under a
//! temporary `$HOME`; never a real host) when binaries are stale: a stale
//! remote `civetd-<version>` is replaced, a stale local dist file is
//! refused before upload, an unsafe remote state dir is refused.

#![cfg(feature = "server")]

use std::path::{Path, PathBuf};
use std::process::Command;

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");
const CLI: &str = env!("CARGO_BIN_EXE_civet-cli");

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

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    client_state: PathBuf,
    dist: PathBuf,
    ssh: PathBuf,
    triple: String,
}

impl Drop for Env {
    fn drop(&mut self) {
        let json = self.home.join(".civet/daemon.json");
        if let Some(info) = std::fs::read_to_string(json)
            .ok()
            .and_then(|s| civet_proto::BootstrapInfo::from_json_line(&s).ok())
        {
            let _ = Command::new("kill").arg(info.pid.to_string()).status();
        }
    }
}

fn uname(flag: &str) -> String {
    let out = Command::new("uname").arg(flag).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn write_exec(p: &Path, body: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn setup() -> Env {
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
    write_exec(&ssh, FAKE_SSH.as_bytes());
    Env {
        home,
        client_state,
        dist,
        ssh,
        triple,
        _tmp: tmp,
    }
}

fn cli(env: &Env, args: &[&str]) -> std::process::Output {
    Command::new(CLI)
        .env("CIVET_STATE_DIR", &env.client_state)
        .env("FAKE_REMOTE_HOME", &env.home)
        .env_remove("CIVET_DIST_DIR")
        .args(["--ssh", "fakehost", "--ssh-program"])
        .arg(&env.ssh)
        .arg("--dist")
        .arg(&env.dist)
        .arg("-v")
        .args(args)
        .output()
        .unwrap()
}

/// A stale binary installed under the current version's name (the bug the
/// user hit: 0.1.0 uploaded as civetd-0.1.1) is detected by the probe's
/// `version` check and replaced.
#[test]
fn stale_remote_binary_is_reinstalled() {
    let env = setup();
    let remote = env
        .home
        .join(format!(".civet/bin/civetd-{}", civetd::VERSION));
    write_exec(
        &remote,
        b"#!/bin/sh\necho 'civetd 0.0.1 (proto 1, old)'\necho '{}'\nexit 0\n",
    );
    let out = cli(&env, &["ping", "--count", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("installed: true"), "stderr: {stderr}");
    let installed = std::fs::read(&remote).unwrap();
    assert_eq!(
        civetd::bootstrap::binary_version(&installed).as_deref(),
        Some(civetd::VERSION)
    );
    // Next time: one round trip, nothing uploaded.
    let out = cli(&env, &["panes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("installed: false"), "stderr: {stderr}");
}

/// A stale local dist file is refused before anything is uploaded, with
/// a message telling the user to rebuild.
#[test]
fn stale_local_binary_is_refused() {
    let env = setup();
    let mut data = std::fs::read(CIVETD).unwrap();
    // Rewrite the embedded version in place (same length).
    let marker = civetd::bootstrap::VERSION_MARKER.as_bytes();
    let pos = data
        .windows(marker.len())
        .position(|w| w == marker)
        .unwrap();
    let old = format!("\0CIVETD_VERSION={}\0", "9".repeat(civetd::VERSION.len()));
    data[pos..pos + marker.len()].copy_from_slice(old.as_bytes());
    write_exec(&env.dist.join(format!("civetd-{}", env.triple)), &data);
    let out = cli(&env, &["ping", "--count", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("build-remote.sh"), "stderr: {stderr}");
    assert!(
        !env.home.join(".civet/bin").exists(),
        "nothing may be uploaded"
    );
}

/// A remote state dir that is a symlink is refused (no upload into it).
#[test]
fn symlinked_remote_state_dir_is_refused() {
    let env = setup();
    let elsewhere = env.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env.home.join(".civet")).unwrap();
    let out = cli(&env, &["ping", "--count", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unsafe remote state directory"),
        "stderr: {stderr}"
    );
    assert!(!elsewhere.join("bin").exists());
}
