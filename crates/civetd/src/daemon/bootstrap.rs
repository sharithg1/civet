//! Server side of `civetd bootstrap`: find (and health-check) the running
//! daemon, replace it when the upgrade policy says so (see the module docs
//! of `daemon`), or start a detached one, then return its bootstrap line.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use civet_proto::{
    BootstrapInfo, ClientHello, ClientId, ClientMessage, ControlRequest, RejectReason, Request,
    Response, ServerMessage,
};
use civet_transport::TransportError;

use super::{ExitError, sys};
use crate::bootstrap::exit_code;
use crate::paths::{StatePaths, create_private_dir};

/// How long the health check may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for a started daemon to become ready.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a SIGTERMed daemon gets to release the lock.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// What the state dir says about a daemon.
#[derive(Debug)]
enum Probe {
    /// No live daemon (no daemon.json, or the lock is free).
    NotRunning,
    /// Live and answering. `panes` is None if the protocol is incompatible.
    Running {
        info: BootstrapInfo,
        panes: Option<usize>,
        compatible: bool,
    },
    /// Holds the lock but daemon.json is missing/stale or the socket does
    /// not answer (starting up, or hung).
    Unresponsive { pid: Option<u32>, why: String },
}

/// Start or reattach to the daemon for `state_dir` and return its line.
pub async fn bootstrap(state_dir: &Path, replace: bool) -> anyhow::Result<BootstrapInfo> {
    let paths = StatePaths::new(state_dir);
    create_private_dir(&paths.root)
        .with_context(|| format!("creating {}", paths.root.display()))?;

    match probe(&paths).await {
        Probe::Running {
            mut info,
            panes,
            compatible,
        } => {
            let ours_newer = version_newer(crate::VERSION, &info.daemon_version);
            let idle = panes == Some(0);
            let keep = !replace && compatible && !(idle && ours_newer);
            if keep {
                info.reattached = true;
                return Ok(info);
            }
            if !replace && !idle {
                return Err(ExitError::new(
                    exit_code::INCOMPATIBLE_BUSY,
                    format!(
                        "running civetd {} (protocol {}, pid {}) is incompatible with {} (protocol {}) \
                         and has live panes; rerun with --replace to stop it",
                        info.daemon_version,
                        info.proto_version,
                        info.pid,
                        crate::VERSION,
                        civet_proto::PROTOCOL_VERSION
                    ),
                )
                .into());
            }
            eprintln!(
                "civetd: replacing running civetd {} (pid {}){}",
                info.daemon_version,
                info.pid,
                if replace {
                    " (--replace)"
                } else {
                    " (idle upgrade)"
                }
            );
            stop_daemon(&paths, info.pid).await?;
        }
        Probe::Unresponsive { pid, why } => {
            if !replace {
                // Maybe it is just starting: give it the start timeout.
                if let Some(info) = wait_ready(&paths).await {
                    return Ok(BootstrapInfo {
                        reattached: true,
                        ..info
                    });
                }
                return Err(ExitError::new(
                    exit_code::ERROR,
                    format!(
                        "a civetd holds the lock but does not answer ({why}); rerun with --replace"
                    ),
                )
                .into());
            }
            match pid {
                Some(pid) => stop_daemon(&paths, pid).await?,
                None => anyhow::bail!("civetd lock held by an unknown process ({why})"),
            }
        }
        Probe::NotRunning => {}
    }
    start_daemon(&paths).await
}

/// Spawn `civetd serve --detach` and wait for it (or a concurrent winner)
/// to be ready.
async fn start_daemon(paths: &StatePaths) -> anyhow::Result<BootstrapInfo> {
    let exe = std::env::current_exe().context("locating the civetd binary")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve")
        .arg("--detach")
        .arg("--state-dir")
        .arg(&paths.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The `--detach` parent exits right after forking; reap it.
    let status = tokio::task::spawn_blocking(move || cmd.status())
        .await
        .context("waiting for civetd serve")?
        .with_context(|| format!("starting {}", exe.display()))?;
    let lost_race = status.code() == Some(exit_code::ALREADY_RUNNING);
    if !status.success() && !lost_race {
        anyhow::bail!("civetd serve --detach failed ({status})");
    }
    match wait_ready(paths).await {
        Some(mut info) => {
            // Lost the race to a concurrent bootstrap: that daemon was not
            // started by us.
            info.reattached = lost_race;
            Ok(info)
        }
        None => Err(anyhow::anyhow!(
            "civetd did not become ready within {:?} (see {})",
            START_TIMEOUT,
            paths.log_file().display()
        )),
    }
}

/// Poll until a healthy daemon is advertised (or the timeout).
async fn wait_ready(paths: &StatePaths) -> Option<BootstrapInfo> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Probe::Running { info, .. } = probe(paths).await {
            return Some(info);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

/// SIGTERM `pid` (graceful: Goodbye to clients), wait for the lock to be
/// released; SIGKILL after the timeout.
async fn stop_daemon(paths: &StatePaths, pid: u32) -> anyhow::Result<()> {
    if let Err(e) = sys::signal(pid, libc::SIGTERM) {
        tracing::warn!(pid, error = %e, "SIGTERM failed");
    }
    if wait_unlocked(paths, STOP_TIMEOUT).await {
        return Ok(());
    }
    eprintln!("civetd: pid {pid} did not stop within {STOP_TIMEOUT:?}; sending SIGKILL");
    let _ = sys::signal(pid, libc::SIGKILL);
    if wait_unlocked(paths, Duration::from_secs(2)).await {
        return Ok(());
    }
    anyhow::bail!("could not stop the running civetd (pid {pid})")
}

async fn wait_unlocked(paths: &StatePaths, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !sys::lock_is_held(&paths.lock()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Inspect the state dir and health-check the advertised daemon.
async fn probe(paths: &StatePaths) -> Probe {
    let info = std::fs::read_to_string(paths.daemon_json())
        .ok()
        .and_then(|s| BootstrapInfo::from_json_line(&s).ok());
    if !sys::lock_is_held(&paths.lock()) {
        return Probe::NotRunning;
    }
    let Some(info) = info else {
        return Probe::Unresponsive {
            pid: None,
            why: "no daemon.json".into(),
        };
    };
    if !sys::pid_alive(info.pid) {
        return Probe::Unresponsive {
            pid: None,
            why: format!("daemon.json names dead pid {}", info.pid),
        };
    }
    let compatible = civet_proto::is_compatible(info.proto_version);
    match tokio::time::timeout(PROBE_TIMEOUT, health_check(paths, &info)).await {
        Ok(Ok(panes)) => Probe::Running {
            info,
            panes: Some(panes),
            compatible,
        },
        Ok(Err(TransportError::Rejected(r))) if r.reason == RejectReason::VersionMismatch => {
            Probe::Running {
                info,
                panes: None,
                compatible: false,
            }
        }
        Ok(Err(e)) => Probe::Unresponsive {
            pid: Some(info.pid),
            why: e.to_string(),
        },
        Err(_) => Probe::Unresponsive {
            pid: Some(info.pid),
            why: "health check timed out".into(),
        },
    }
}

/// hello + Ping + ListPanes over the unix socket; returns the pane count.
async fn health_check(paths: &StatePaths, info: &BootstrapInfo) -> Result<usize, TransportError> {
    let stream = tokio::net::UnixStream::connect(paths.socket()).await?;
    let client_id = ClientId::generate()?;
    let hello = ClientHello::new(
        info.session_key.clone(),
        client_id,
        "civetd-bootstrap",
        crate::VERSION,
    );
    let pipelined = vec![
        ClientMessage::Request(ControlRequest {
            id: 1,
            request: Request::Ping { nonce: 1 },
        }),
        ClientMessage::Request(ControlRequest {
            id: 2,
            request: Request::ListPanes,
        }),
    ];
    let connected = civet_transport::connect_stream(stream, hello, pipelined).await?;
    let mut rx = connected.incoming;
    let (mut pong, mut panes) = (false, None);
    while !(pong && panes.is_some()) {
        match rx.recv().await? {
            Some(ServerMessage::Response(r)) => match (r.id, r.result) {
                (1, Ok(Response::Pong { .. })) => pong = true,
                (2, Ok(Response::Panes(p))) => panes = Some(p.len()),
                (id, other) => {
                    connected.conn.close().await;
                    return Err(TransportError::ProtocolViolation(format!(
                        "unexpected health-check reply {id}: {other:?}"
                    )));
                }
            },
            Some(_) => {}
            None => return Err(TransportError::Closed("daemon closed the probe".into())),
        }
    }
    connected.conn.close().await;
    Ok(panes.unwrap_or(0))
}

/// True if semver-ish `a` is strictly newer than `b` (numeric components;
/// anything unparsable counts as 0).
pub(crate) fn version_newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    parts(a) > parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(version_newer("1.0.0", "0.9.9"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.2.0"));
        assert!(version_newer("0.1.10", "0.1.9"));
    }
}
