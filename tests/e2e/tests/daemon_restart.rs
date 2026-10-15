//! Scenario 9: the daemon restarts while a client is connected (real
//! `civetd serve` binary, fixed UDP port so the restarted daemon is
//! reachable at the same address).
//!
//! * crash (SIGKILL): the session loses the link, keeps re-dialing, and
//!   when the new daemon is up its hello is rejected with `BadKey` (the
//!   session key is per daemon instance) → `LinkState::Failed {
//!   needs_bootstrap: true }`;
//! * graceful (SIGTERM): the daemon says `Goodbye` first → same `Failed`.
//!
//! Then the client does what civet-desktop's `HostManager::rebootstrap`
//! does: bootstrap again (read the new `daemon.json`), connect a new
//! session, compare `instance_id` → changed, so it drops its pane views:
//! the old pane id is unknown to the new daemon, and a new pane works.
//!
//! Note: the transport-level `SessionEvent::Connected { instance_changed:
//! true }` cannot fire here, because a restarted daemon always has a new
//! session key; the instance change is detected one layer up (desktop),
//! which this test replicates.

use std::time::Duration;

use civet_e2e::*;
use civet_proto::{BootstrapInfo, ClientId, ErrorCode, Request, Response};
use civet_transport::{LinkState, RequestError};
use tokio::time::Instant;

async fn restart(graceful: bool) {
    let _serial = serial().await;
    init_logging();
    let label = if graceful { "graceful" } else { "crash" };
    let dir = tempfile::tempdir().unwrap();
    let port = free_udp_port();
    let d1 = ChildDaemon::start(dir.path(), port).await.unwrap();
    let old_info = d1.info.clone();
    let cid = ClientId::generate().unwrap();
    let (_q, dialer) = quic_dialer(d1.addr(), old_info.cert_fingerprint, Duration::from_secs(5));
    let mut c = Client::connect(dialer, &old_info.session_key, cid)
        .await
        .unwrap();
    let old_instance = c.first_hello.instance_id;
    let pane = c.spawn(sh_pane(dir.path())).await;
    c.attach(pane).await;
    c.echo_rtt(pane, "before-restart", WAIT).await;

    let t0 = Instant::now();
    if graceful {
        d1.terminate().await;
    } else {
        d1.kill().await;
    }
    let d2 = ChildDaemon::start(dir.path(), port).await.unwrap();
    let new_up = t0.elapsed();
    assert_ne!(d2.info.session_key, old_info.session_key);
    assert_eq!(
        d2.info.cert_fingerprint, old_info.cert_fingerprint,
        "the TLS identity persists across restarts"
    );

    c.wait_until(Duration::from_secs(30), "Failed { needs_bootstrap }", |c| {
        matches!(
            *c.session.link_state().borrow(),
            LinkState::Failed {
                needs_bootstrap: true,
                ..
            }
        )
    })
    .await;
    let failed_at = t0.elapsed();
    c.pump_for(Duration::from_millis(300)).await; // drain queued events
    if graceful {
        assert!(
            c.goodbye.is_some(),
            "a graceful stop says Goodbye; links: {:?}",
            c.links
        );
    }
    assert!(
        matches!(
            c.session.request(Request::Ping { nonce: 1 }).await,
            Err(RequestError::Closed)
        ),
        "the failed session refuses requests"
    );

    // Re-bootstrap: the new daemon.json, a new session.
    let t_reboot = Instant::now();
    let json = civetd::paths::StatePaths::new(dir.path()).daemon_json();
    let info = BootstrapInfo::from_json_line(&std::fs::read_to_string(json).unwrap()).unwrap();
    assert_eq!(info.pid, d2.pid());
    let (_q2, dialer2) = quic_dialer(d2.addr(), info.cert_fingerprint, Duration::from_secs(5));
    let mut c2 = Client::connect(dialer2, &info.session_key, cid)
        .await
        .unwrap();
    let instance_changed = c2.first_hello.instance_id != old_instance;
    assert!(instance_changed, "a restarted daemon has a new instance id");
    assert!(!c2.first_hello.resumed);
    match c2.session.request(Request::ListPanes).await.unwrap() {
        Response::Panes(p) => assert!(p.is_empty(), "panes died with the old daemon: {p:?}"),
        other => panic!("{other:?}"),
    }
    match c2.session.subscribe(pane).await {
        Err(RequestError::Rpc(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("old pane id must be unknown: {other:?}"),
    }
    let p2 = c2.spawn(sh_pane(dir.path())).await;
    c2.attach(p2).await;
    c2.echo_rtt(p2, "after-restart", WAIT).await;
    let live = t_reboot.elapsed();
    eprintln!(
        "daemon restart ({label}): new daemon ready {:.0} ms, client Failed(needs_bootstrap) at {:.0} ms, \
         re-bootstrap -> new pane echo {:.0} ms; instance changed {instance_changed}",
        ms(new_up),
        ms(failed_at),
        ms(live)
    );
    record(
        &format!("daemon_restart.{label}.detected"),
        ms(failed_at.saturating_sub(new_up)),
        "ms",
        serde_json::json!({ "new_daemon_ready_ms": ms(new_up), "rebootstrap_to_echo_ms": ms(live) }),
    );
    c2.session.close().await;
    d2.terminate().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_crash_restart() {
    restart(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_graceful_restart() {
    restart(true).await;
}
