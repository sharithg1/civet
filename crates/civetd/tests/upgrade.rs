//! Daemon upgrade with live panes (temp state dirs only): an older
//! compatible daemon with panes is reattached and reported as
//! `upgrade_available`; bootstrap with `replace` starts a new instance.
//! Also checks that a locally bootstrapped daemon binds loopback only.

#![cfg(feature = "server")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use civet_proto::BootstrapInfo;
use civetd::bootstrap::{bootstrap_local, bootstrap_local_with};
use civetd::paths::StatePaths;

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");
const CLI: &str = env!("CARGO_BIN_EXE_civet-cli");

/// Stops whatever daemon the state dir advertises, even on failure.
struct StopOnDrop(PathBuf);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(info) = read_info(&self.0) {
            let _ = Command::new("kill").arg(info.pid.to_string()).status();
        }
    }
}

fn read_info(dir: &Path) -> Option<BootstrapInfo> {
    let s = std::fs::read_to_string(StatePaths::new(dir).daemon_json()).ok()?;
    BootstrapInfo::from_json_line(&s).ok()
}

fn cmdline(pid: u32) -> String {
    let out = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_available_with_live_panes_then_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let _stop = StopOnDrop(dir.clone());
    let civetd = Path::new(CIVETD);

    // Fresh start: loopback only, annotated line.
    let a = bootstrap_local(civetd, Some(&dir)).await.unwrap();
    assert!(!a.info.reattached);
    assert_eq!(a.info.available_version.as_deref(), Some(civetd::VERSION));
    assert_eq!(a.info.live_panes, Some(0));
    assert!(!a.info.upgrade_available);
    let args = cmdline(a.info.pid);
    assert!(args.contains("--bind 127.0.0.1"), "serve args: {args}");

    // A live pane.
    let out = Command::new(CLI)
        .args(["--local", "--civetd", CIVETD, "--state-dir"])
        .arg(&dir)
        .args(["spawn", "--", "sleep", "300"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Pretend the running daemon is an older (compatible) release.
    let paths = StatePaths::new(&dir);
    let mut old = read_info(&dir).unwrap();
    old.daemon_version = "0.0.1".into();
    std::fs::write(paths.daemon_json(), old.to_json_line()).unwrap();

    // Reattach (panes are protected), and say an upgrade is available.
    let b = bootstrap_local(civetd, Some(&dir)).await.unwrap();
    assert!(b.info.reattached);
    assert_eq!(b.info.pid, a.info.pid);
    assert_eq!(b.info.daemon_version, "0.0.1");
    assert_eq!(b.info.available_version.as_deref(), Some(civetd::VERSION));
    assert_eq!(b.info.live_panes, Some(1));
    assert!(b.info.upgrade_available);

    // Replace through the client library: a new instance.
    let c = bootstrap_local_with(civetd, Some(&dir), true)
        .await
        .unwrap();
    assert!(!c.info.reattached);
    assert_ne!(c.info.pid, a.info.pid);
    assert_eq!(c.info.daemon_version, civetd::VERSION);
    assert_eq!(c.info.live_panes, Some(0));
    assert!(!c.info.upgrade_available);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Command::new("kill")
        .args(["-0", &a.info.pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < deadline, "old daemon still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The binary carries its version marker (the client checks it before
/// uploading).
#[test]
fn binary_has_version_marker() {
    let data = std::fs::read(CIVETD).unwrap();
    assert_eq!(
        civetd::bootstrap::binary_version(&data).as_deref(),
        Some(civetd::VERSION)
    );
}
