//! Listening-port updates and TCP port forwarding through an in-process
//! daemon over real QUIC (and their refusal on the stream link).

#![cfg(feature = "server")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use civet_proto::{BootstrapInfo, ClientId, ErrorCode, PortsUpdate};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, PortsWatch, QuicClient, QuicClientConfig,
    QuicDialer, RequestError, StreamDialer,
};
use civetd::daemon::{Daemon, DaemonOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");
const WAIT: Duration = Duration::from_secs(15);

async fn start_daemon() -> (Daemon, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut o = DaemonOptions::new(dir.path());
    o.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    o.port_range = 0..=0;
    o.shell = Some("/bin/sh".into());
    (Daemon::start(o).await.unwrap(), dir)
}

async fn session(dialer: Arc<dyn Dialer>, info: &BootstrapInfo) -> ClientSession {
    let cfg = ClientSessionConfig::new(
        info.session_key.clone(),
        ClientId::generate().unwrap(),
        "ports-test",
        "0",
    );
    ClientSession::connect(cfg, dialer).await.unwrap().0
}

async fn quic_session(info: &BootstrapInfo) -> ClientSession {
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client: Arc::new(QuicClient::new(false).unwrap()),
        config: QuicClientConfig::new(
            SocketAddr::from(([127, 0, 0, 1], info.udp_port)),
            info.cert_fingerprint,
        ),
    });
    session(dialer, info).await
}

/// Next update that satisfies `pred`.
async fn wait_update(watch: &mut PortsWatch, pred: impl Fn(&PortsUpdate) -> bool) -> PortsUpdate {
    tokio::time::timeout(WAIT, async {
        loop {
            let u = watch.next().await.unwrap().expect("ports stream ended");
            if pred(&u) {
                return u;
            }
        }
    })
    .await
    .expect("timed out waiting for a ports update")
}

/// One-connection echo server; returns its port.
async fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_sees_listeners_come_and_go() {
    let (daemon, _dir) = start_daemon().await;
    let s = quic_session(daemon.info()).await;

    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_port = first.local_addr().unwrap().port();
    let mut watch = s.watch_ports().await.unwrap();
    let u = wait_update(&mut watch, |u| u.ports.iter().any(|p| p.port == first_port)).await;
    let p = u.ports.iter().find(|p| p.port == first_port).unwrap();
    assert!(p.addrs.iter().any(|a| a == "127.0.0.1"), "{p:?}");
    // Our own process: found, but not in any pane.
    assert_eq!(p.pid, Some(std::process::id()), "{p:?}");
    assert_eq!(p.pane, None);

    let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_port = second.local_addr().unwrap().port();
    wait_update(&mut watch, |u| {
        u.ports.iter().any(|p| p.port == second_port)
    })
    .await;

    drop(first);
    wait_update(&mut watch, |u| {
        !u.ports.iter().any(|p| p.port == first_port)
    })
    .await;
    drop(second);

    s.close().await;
    daemon.shutdown("test done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forward_round_trip_and_refusal() {
    let (daemon, _dir) = start_daemon().await;
    let s = quic_session(daemon.info()).await;
    let port = echo_server().await;

    // Several connections at once, each with its own bytes, half-closed by
    // the client: everything comes back, then EOF.
    let mut tasks = Vec::new();
    for i in 0..8u8 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            let mut t = s.open_forward(port).await.unwrap();
            let data: Vec<u8> = (0..200_000u32).map(|n| (n as u8).wrapping_add(i)).collect();
            let (mut r, mut w) = tokio::io::split(&mut t);
            let send = async {
                w.write_all(&data).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let mut back = Vec::new();
            let recv = r.read_to_end(&mut back);
            let ((), got) = tokio::join!(send, recv);
            got.unwrap();
            assert_eq!(back, data);
        }));
    }
    for t in tasks {
        tokio::time::timeout(WAIT, t).await.unwrap().unwrap();
    }

    // Nothing listens there any more.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    match s.open_forward(closed).await {
        Err(RequestError::Rpc(e)) => assert_eq!(e.code, ErrorCode::NotFound, "{e}"),
        other => panic!("expected NotFound, got {other:?}"),
    }
    // The connection is still fine afterwards.
    s.open_forward(port).await.unwrap();

    s.close().await;
    daemon.shutdown("test done").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_link_reports_unsupported() {
    let (daemon, dir) = start_daemon().await;
    let state = dir.path().to_path_buf();
    let dialer: Arc<dyn Dialer> = Arc::new(StreamDialer {
        factory: move || {
            let state = state.clone();
            async move {
                civetd::bootstrap::open_local_tunnel(Path::new(CIVETD), Some(&state))
                    .await
                    .map_err(std::io::Error::other)
            }
        },
    });
    let s = session(dialer, daemon.info()).await;
    match s.watch_ports().await {
        Err(RequestError::Rpc(e)) => assert_eq!(e.code, ErrorCode::Unsupported),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    match s.open_forward(1).await {
        Err(RequestError::Rpc(e)) => assert_eq!(e.code, ErrorCode::Unsupported),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    s.close().await;
    daemon.shutdown("test done").await;
}
