//! End-to-end through the production `SshConnector` in local mode: a real
//! `civetd bootstrap` child process (detached daemon in a temp state dir),
//! QUIC, then the `civetd stdio-tunnel` link when forced.
//!
//! Needs a built daemon binary, so it is ignored by default:
//! `cargo build -p civetd && cargo test -p civet-desktop --test local_civetd -- --ignored`
//! (or point `CIVET_TEST_CIVETD` at a civetd binary).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use civet_desktop::connect::SshConnector;
use civet_desktop::events::{
    AgentAttentionEvent, BootstrapProgressEvent, DaemonEventPayload, HostStateEvent, HostTarget,
    TransportChoice, UiEvents,
};
use civet_desktop::hosts::{HostManager, ManagerConfig};
use civet_desktop::records::decode_all;
use civet_proto::{BootstrapInfo, ClientId, CommandSpec, SpawnPaneRequest, TermSize};
use civet_transport::TransportKind;
use tokio::sync::mpsc;

struct NullUi;

impl UiEvents for NullUi {
    fn host_state(&self, _: HostStateEvent) {}
    fn bootstrap_progress(&self, _: BootstrapProgressEvent) {}
    fn daemon_event(&self, _: DaemonEventPayload) {}
    fn agent_attention(&self, _: AgentAttentionEvent) {}
    fn window_focused(&self) -> bool {
        true
    }
    fn notify(&self, _: &str, _: &str) {}
}

fn civetd_binary() -> PathBuf {
    std::env::var_os("CIVET_TEST_CIVETD")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/civetd")
        })
}

async fn echo_roundtrip(mgr: &HostManager, marker: &str) {
    let pane = mgr
        .spawn_pane(
            "local",
            SpawnPaneRequest {
                worktree: None,
                cwd: Some("/tmp".into()),
                command: Some(CommandSpec::new("/bin/cat", Vec::<String>::new())),
                env: Vec::new(),
                size: TermSize::new(80, 24),
                title: None,
                agent: None,
            },
        )
        .await
        .unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    mgr.attach_pane("local", pane.id, Arc::new(tx))
        .await
        .unwrap();
    mgr.write_input("local", pane.id, format!("{marker}\r").into_bytes())
        .unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !String::from_utf8_lossy(&seen).contains(marker) {
            let msg = rx.recv().await.unwrap();
            for (_, p) in decode_all(&msg).unwrap() {
                seen.extend_from_slice(p);
            }
        }
    })
    .await
    .expect("echo");
    mgr.kill_pane("local", pane.id, None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a built civetd binary (cargo build -p civetd)"]
async fn local_mode_quic_then_stdio_tunnel() {
    let civetd = civetd_binary();
    assert!(civetd.is_file(), "no civetd at {}", civetd.display());
    let dir = tempfile::Builder::new()
        .prefix("cvl")
        .tempdir_in("/tmp")
        .unwrap();
    let connector = Arc::new(SshConnector {
        client_id: ClientId::generate().unwrap(),
        local_civetd: Some(civetd),
        local_state_dir: Some(dir.path().to_path_buf()),
        bundled_dist: None,
    });
    let mgr = HostManager::new(
        ManagerConfig::default(),
        Arc::new(NullUi),
        connector,
        tokio::runtime::Handle::current(),
    );

    let s = mgr
        .connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    assert_eq!(s.transport, Some(TransportKind::Quic));
    echo_roundtrip(&mgr, "over-quic").await;
    mgr.disconnect_host("local").await.unwrap();

    // Forced tunnel: reattaches to the same daemon over `civetd stdio-tunnel`.
    let s2 = mgr
        .connect_host(HostTarget::Local, TransportChoice::Ssh)
        .await
        .unwrap();
    assert_eq!(s2.transport, Some(TransportKind::Stream));
    assert_eq!(s2.instance_id, s.instance_id, "reattached, not restarted");
    echo_roundtrip(&mgr, "over-tunnel").await;
    // Same version: no upgrade offered.
    assert_eq!(s2.upgrade, None);

    // restart_daemon: `civetd bootstrap --replace` stops the daemon even
    // though it has a live pane and starts a new instance; the reconnect
    // reuses the stored target and transport choice (the tunnel).
    mgr.spawn_pane(
        "local",
        SpawnPaneRequest {
            worktree: None,
            cwd: Some("/tmp".into()),
            command: Some(CommandSpec::new("/bin/cat", Vec::<String>::new())),
            env: Vec::new(),
            size: TermSize::new(80, 24),
            title: None,
            agent: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(mgr.list_panes("local").await.unwrap().len(), 1);
    let s3 = mgr.restart_daemon("local").await.unwrap();
    assert_ne!(s3.instance_id, s2.instance_id, "a new daemon instance");
    assert_eq!(s3.transport, Some(TransportKind::Stream));
    assert!(mgr.list_panes("local").await.unwrap().is_empty());
    echo_roundtrip(&mgr, "after-restart").await;
    mgr.disconnect_host("local").await.unwrap();

    // Stop the detached daemon.
    let info = std::fs::read_to_string(dir.path().join("daemon.json")).unwrap();
    let pid = BootstrapInfo::from_json_line(&info).unwrap().pid;
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
}

/// The packaged app's Local path: `SshConnector::new` with the app's resource
/// dir finds the bundled `civet-dist/civetd-<this triple>` (no CIVET_CIVETD,
/// no local_civetd) and bootstraps it. Uses `CIVET_TEST_RESOURCES` (e.g.
/// `target/release/bundle/macos/Civet.app/Contents/Resources`) or the
/// `crates/civet-desktop` dir, whose `civet-dist/` `scripts/build-remote.sh
/// --bundle` fills.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs bundled daemons (scripts/build-remote.sh --bundle)"]
async fn local_mode_uses_the_bundled_civetd() {
    let resources = std::env::var_os("CIVET_TEST_RESOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let dist = resources.join("civet-dist");
    let bundled = civet_desktop::connect::bundled_local_civetd(&dist).unwrap_or_else(|| {
        panic!(
            "no matching civetd-{} in {}",
            civetd::TARGET,
            dist.display()
        )
    });
    assert!(
        std::env::var_os("CIVET_CIVETD").is_none(),
        "unset CIVET_CIVETD"
    );
    assert_eq!(
        civet_desktop::connect::find_local_civetd(Some(&dist)),
        Some(bundled)
    );
    let dir = tempfile::Builder::new()
        .prefix("cvb")
        .tempdir_in("/tmp")
        .unwrap();
    let mut connector = SshConnector::new(ClientId::generate().unwrap(), Some(&resources));
    connector.local_state_dir = Some(dir.path().to_path_buf());
    let mgr = HostManager::new(
        ManagerConfig::default(),
        Arc::new(NullUi),
        Arc::new(connector),
        tokio::runtime::Handle::current(),
    );
    let s = mgr
        .connect_host(HostTarget::Local, TransportChoice::Auto)
        .await
        .unwrap();
    assert_eq!(s.daemon_version.as_deref(), Some(civetd::VERSION));
    echo_roundtrip(&mgr, "bundled").await;
    mgr.disconnect_host("local").await.unwrap();
    let info = std::fs::read_to_string(dir.path().join("daemon.json")).unwrap();
    let pid = BootstrapInfo::from_json_line(&info).unwrap().pid;
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
}
