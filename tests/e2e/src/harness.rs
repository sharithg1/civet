//! Daemons (in-process and real binary), dialers, and a client wrapper that
//! keeps per-pane mirrors up to date.
//!
//! Every daemon lives in a fresh temp state dir (never `~/.civet`) and is
//! torn down even when a test fails: in-process daemons kill their panes
//! on drop ([`DaemonGuard`]), child daemons are SIGKILLed on drop
//! ([`ChildDaemon`]), stdio tunnels are `kill_on_drop` children.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use civet_proto::{
    BootstrapInfo, CertFingerprint, ClientHello, ClientId, ClientMessage, CommandSpec,
    ControlRequest, KillSignal, PaneId, Request, Response, RpcError, ScreenAck, ScrollbackQuery,
    ScrollbackRequest, ServerHello, ServerMessage, SessionKey, SpawnPaneRequest, TermSize,
};
use civet_transport::{
    ClientConnection, ClientReceiver, ClientSession, ClientSessionConfig, Connected, Dialer,
    LinkState, QuicClient, QuicClientConfig, QuicDialer, SessionEvent, StreamDialer,
    TransportError,
};
use civetd::daemon::{Daemon, DaemonOptions};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::mirror::{Mirror, strip_vt};

/// Default wait for anything a test expects to happen.
pub const WAIT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Process-wide helpers
// ---------------------------------------------------------------------------

/// Timing-sensitive tests in one binary run one at a time (a tokio mutex:
/// works across the per-test runtimes and is not poisoned by a failure).
pub async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// Opt-in logs: `CIVET_E2E_LOG=civet_transport=debug,info`.
pub fn init_logging() {
    if let Ok(filter) = std::env::var("CIVET_E2E_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(std::io::stderr)
            .try_init();
    }
}

/// Path of the real `civetd` binary, built on first use with the same
/// profile as this test (`cargo build -p civetd --bin civetd [--release]`).
/// Override with `CIVETD_BIN`.
pub fn civetd_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = std::env::var("CIVETD_BIN") {
            return PathBuf::from(p);
        }
        let exe = std::env::current_exe().expect("current_exe");
        // target/<profile>/deps/<test-binary>
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("test binary is in target/<profile>/deps")
            .to_path_buf();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut cmd = std::process::Command::new(env!("CARGO"));
        cmd.current_dir(&workspace)
            .args(["build", "--quiet", "-p", "civetd", "--bin", "civetd"]);
        if !cfg!(debug_assertions) {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("running cargo build for civetd");
        assert!(status.success(), "building civetd failed");
        let bin = profile_dir.join("civetd");
        assert!(bin.exists(), "{} missing after build", bin.display());
        bin
    })
    .clone()
}

/// A UDP port that was free a moment ago.
pub fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .expect("binding a probe socket")
}

/// `ps -o rss=` of a process, in KiB.
pub fn rss_kib(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Direct children of a process (`pgrep -P`).
pub fn child_pids(pid: u32) -> Vec<u32> {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Send a signal with `kill(1)` (no unsafe code in this crate).
pub fn signal(pid: u32, sig: &str) -> bool {
    std::process::Command::new("kill")
        .args([format!("-{sig}"), pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

// ---------------------------------------------------------------------------
// Daemons
// ---------------------------------------------------------------------------

/// In-process daemon on loopback in a temp state dir. Panes are `/bin/sh`.
/// Call [`DaemonGuard::shutdown`] at the end; on a panic, drop kills every
/// pane so no shell outlives the test.
#[derive(Debug)]
pub struct DaemonGuard {
    daemon: Option<Daemon>,
    /// State dir (also a scratch dir for test files).
    pub dir: tempfile::TempDir,
}

impl DaemonGuard {
    /// Start a daemon.
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut o = DaemonOptions::new(dir.path());
        o.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        o.port_range = 0..=0;
        o.shell = Some("/bin/sh".into());
        let daemon = Daemon::start(o).await.expect("daemon start");
        Self {
            daemon: Some(daemon),
            dir,
        }
    }

    /// The daemon.
    pub fn d(&self) -> &Daemon {
        self.daemon.as_ref().expect("daemon running")
    }

    /// Its bootstrap line.
    pub fn info(&self) -> BootstrapInfo {
        self.d().info().clone()
    }

    /// QUIC address.
    pub fn quic_addr(&self) -> SocketAddr {
        self.d().quic_addr()
    }

    /// Server-side screen of a pane.
    pub fn screen(&self, pane: PaneId) -> Vec<String> {
        self.d()
            .sessions()
            .screen_text(pane)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.trim_end().to_string())
            .collect()
    }

    /// Graceful shutdown.
    pub async fn shutdown(mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown("test done").await;
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(d) = &self.daemon {
            for p in d.sessions().list_panes() {
                let _ = d.sessions().kill_pane(p.id, KillSignal::Kill);
            }
        }
    }
}

/// The real `civetd serve` binary as a child process (SIGKILL on drop).
#[derive(Debug)]
pub struct ChildDaemon {
    child: tokio::process::Child,
    /// Its bootstrap line (from `daemon.json`).
    pub info: BootstrapInfo,
    /// State dir.
    pub state_dir: PathBuf,
    /// UDP port.
    pub port: u16,
}

impl ChildDaemon {
    /// `civetd serve --state-dir DIR --bind 127.0.0.1 --port-range P-P`,
    /// panes use `/bin/sh`. Waits for `daemon.json`.
    pub async fn start(state_dir: &Path, port: u16) -> anyhow::Result<Self> {
        let json = civetd::paths::StatePaths::new(state_dir).daemon_json();
        let _ = std::fs::remove_file(&json);
        let log = std::fs::File::create(state_dir.join(format!("serve-{port}.log")))?;
        let child = tokio::process::Command::new(civetd_bin())
            .arg("serve")
            .arg("--state-dir")
            .arg(state_dir)
            .args(["--bind", "127.0.0.1", "--port-range"])
            .arg(format!("{port}-{port}"))
            .env("SHELL", "/bin/sh")
            .env("CIVET_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id().unwrap_or(0);
        let deadline = Instant::now() + Duration::from_secs(15);
        let info = loop {
            if let Some(info) = std::fs::read_to_string(&json)
                .ok()
                .and_then(|s| BootstrapInfo::from_json_line(&s).ok())
                && info.pid == pid
            {
                break info;
            }
            anyhow::ensure!(Instant::now() < deadline, "civetd serve did not get ready");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        Ok(Self {
            child,
            info,
            state_dir: state_dir.to_path_buf(),
            port,
        })
    }

    /// Process id.
    pub fn pid(&self) -> u32 {
        self.info.pid
    }

    /// QUIC address.
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// Crash: SIGKILL and reap.
    pub async fn kill(mut self) {
        let _ = self.child.kill().await;
    }

    /// Graceful: SIGTERM and wait (SIGKILL after 10 s).
    pub async fn terminate(mut self) {
        signal(self.pid(), "TERM");
        if tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Dialers
// ---------------------------------------------------------------------------

/// QUIC dialer to `addr` on a fresh client endpoint.
pub fn quic_dialer(
    addr: SocketAddr,
    fingerprint: CertFingerprint,
    connect_timeout: Duration,
) -> (Arc<QuicClient>, Arc<dyn Dialer>) {
    let client = Arc::new(QuicClient::new(false).expect("client endpoint"));
    let mut config = QuicClientConfig::new(addr, fingerprint);
    config.connect_timeout = connect_timeout;
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client: client.clone(),
        config,
    });
    (client, dialer)
}

/// Stdio-tunnel dialer: every dial spawns the real `civetd stdio-tunnel`
/// for `state_dir` (the local equivalent of `ssh host civetd stdio-tunnel`).
/// Spawned tunnel pids are pushed to the returned list.
pub fn tunnel_dialer(state_dir: &Path) -> (Arc<dyn Dialer>, Arc<Mutex<Vec<u32>>>) {
    let state = state_dir.to_path_buf();
    let pids = Arc::new(Mutex::new(Vec::new()));
    let pids2 = pids.clone();
    let bin = civetd_bin();
    let dialer: Arc<dyn Dialer> = Arc::new(StreamDialer {
        factory: move || {
            let state = state.clone();
            let bin = bin.clone();
            let pids = pids2.clone();
            async move {
                let t = civetd::bootstrap::open_local_tunnel(&bin, Some(&state))
                    .await
                    .map_err(std::io::Error::other)?;
                if let Some(pid) = t.pid() {
                    pids.lock().unwrap_or_else(|p| p.into_inner()).push(pid);
                }
                Ok(t)
            }
        },
    });
    (dialer, pids)
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// `sh` pane (PS1 `$ `) in `cwd`, 80x24.
pub fn sh_pane(cwd: &Path) -> Request {
    cmd_pane(cwd, "sh", &[])
}

/// Pane running `program args…` in `cwd`, 80x24.
pub fn cmd_pane(cwd: &Path, program: &str, args: &[&str]) -> Request {
    Request::SpawnPane(SpawnPaneRequest {
        worktree: None,
        cwd: Some(cwd.display().to_string()),
        command: Some(CommandSpec::new(
            program,
            args.iter().map(|s| s.to_string()),
        )),
        env: vec![("PS1".into(), "$ ".into())],
        size: TermSize::new(80, 24),
        title: None,
        agent: None,
    })
}

// ---------------------------------------------------------------------------
// Client (ClientSession + mirrors)
// ---------------------------------------------------------------------------

/// A `ClientSession` plus everything it reported.
#[derive(Debug)]
pub struct Client {
    /// The session.
    pub session: ClientSession,
    /// Its events.
    pub events: UnboundedReceiver<SessionEvent>,
    /// Per-pane mirrors (created by [`Client::attach`]).
    pub mirrors: HashMap<PaneId, Mirror>,
    /// `(resumed, instance_changed)` of every reconnect after the first
    /// connection.
    pub reconnects: Vec<(bool, bool)>,
    /// The session's `Connected` event for the first connection was seen.
    first_connected_seen: bool,
    /// Link state changes with their time.
    pub links: Vec<(Instant, LinkState)>,
    /// Goodbye reason, if any.
    pub goodbye: Option<String>,
    /// The first hello.
    pub first_hello: ServerHello,
    /// Track transcripts in new mirrors.
    pub transcripts: bool,
}

/// Session config with a client id (default timings).
pub fn session_config(key: &SessionKey, client_id: ClientId) -> ClientSessionConfig {
    ClientSessionConfig::new(key.clone(), client_id, "civet-e2e", "0")
}

impl Client {
    /// First connect (one dial, no retries).
    pub async fn connect(
        dialer: Arc<dyn Dialer>,
        key: &SessionKey,
        client_id: ClientId,
    ) -> Result<Self, TransportError> {
        let (session, events) =
            ClientSession::connect(session_config(key, client_id), dialer).await?;
        let first_hello = session.server_hello();
        Ok(Self {
            session,
            events,
            mirrors: HashMap::new(),
            reconnects: Vec::new(),
            first_connected_seen: false,
            links: Vec::new(),
            goodbye: None,
            first_hello,
            transcripts: true,
        })
    }

    /// Handle one event.
    pub fn handle(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Screen(u) => {
                let t = self.transcripts;
                self.mirrors
                    .entry(u.pane)
                    .or_insert_with(|| Mirror::new(t))
                    .apply(&u);
            }
            SessionEvent::Connected {
                resumed,
                instance_changed,
                ..
            } => {
                if self.first_connected_seen {
                    self.reconnects.push((resumed, instance_changed));
                } else {
                    self.first_connected_seen = true;
                }
            }
            SessionEvent::Link(l) => self.links.push((Instant::now(), l)),
            SessionEvent::Goodbye { reason } => self.goodbye = Some(reason),
            SessionEvent::InputAcked { .. } | SessionEvent::Event(_) => {}
        }
    }

    /// Process events until `pred` holds or `timeout` passes.
    pub async fn wait_until(
        &mut self,
        timeout: Duration,
        what: &str,
        mut pred: impl FnMut(&Client) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        while !pred(self) {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(ev)) => self.handle(ev),
                Ok(None) => panic!("session ended while waiting for {what}"),
                Err(_) => panic!(
                    "timed out after {timeout:?} waiting for {what}; link={:?}; screens={:?}",
                    *self.session.link_state().borrow(),
                    self.mirrors
                        .iter()
                        .map(|(p, m)| (p.0, m.state, crate::mirror::trim_rows(&m.rows())))
                        .collect::<Vec<_>>()
                ),
            }
        }
    }

    /// Like [`Client::wait_until`] but returns false on timeout.
    pub async fn try_wait(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&Client) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while !pred(self) {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(ev)) => self.handle(ev),
                Ok(None) | Err(_) => return pred(self),
            }
        }
        true
    }

    /// Process whatever arrives for `d`.
    pub async fn pump_for(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            self.handle(ev);
        }
    }

    /// Spawn a pane.
    pub async fn spawn(&self, req: Request) -> PaneId {
        match self.session.request(req).await.expect("spawn request") {
            Response::PaneSpawned(p) => p.id,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Subscribe and create its mirror.
    pub async fn attach(&mut self, pane: PaneId) {
        let t = self.transcripts;
        self.mirrors.entry(pane).or_insert_with(|| Mirror::new(t));
        self.session.subscribe(pane).await.expect("subscribe");
    }

    /// The mirror of a pane.
    pub fn m(&self, pane: PaneId) -> &Mirror {
        &self.mirrors[&pane]
    }

    /// True if the pane's mirror shows `row`.
    pub fn shows(&self, pane: PaneId, row: &str) -> bool {
        self.mirrors.get(&pane).is_some_and(|m| m.has_row(row))
    }

    /// Run `echo <word>` and measure until `<word>` is on the client's
    /// screen (keystroke → echo, as the user would see it).
    pub async fn echo_rtt(&mut self, pane: PaneId, word: &str, timeout: Duration) -> Duration {
        let t0 = Instant::now();
        self.session
            .send_input(pane, format!("echo {word}\n").into_bytes());
        self.wait_until(timeout, &format!("echo {word}"), |c| c.shows(pane, word))
            .await;
        t0.elapsed()
    }

    /// Wait until the mirror's screen equals the server's.
    pub async fn converge(
        &mut self,
        pane: PaneId,
        server: impl Fn() -> Vec<String>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let want = server();
            if self.mirrors.get(&pane).is_some_and(|m| {
                m.rows()
                    .iter()
                    .map(|r| r.trim_end().to_string())
                    .collect::<Vec<_>>()
                    == want
            }) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "client screen never converged to the server's\nclient: {:?}\nserver: {:?}",
                    self.mirrors.get(&pane).map(|m| m.rows()),
                    want
                );
            }
            self.pump_for(Duration::from_millis(20)).await;
        }
    }

    /// Everything the pane printed: transcript lines (gaps filled with
    /// `fetch_scrollback`, in pages of 1000) followed by the visible rows.
    /// Returns the lines and how many were fetched.
    pub async fn full_history(&mut self, pane: PaneId) -> (Vec<String>, u64) {
        let missing = self.m(pane).missing_ranges();
        let mut fetched: Vec<(u64, String)> = Vec::new();
        for (start, end) in missing {
            let mut s = start;
            while s < end {
                let e = (s + 1000).min(end);
                let page = self
                    .session
                    .fetch_scrollback(pane, ScrollbackQuery::Range { start: s, end: e })
                    .await
                    .expect("fetch_scrollback");
                assert_eq!(
                    page.start, s,
                    "history {s}..{e} was evicted (oldest {})",
                    page.oldest_available
                );
                for (i, l) in page.lines.iter().enumerate() {
                    fetched.push((page.start + i as u64, strip_vt(&l.vt)));
                }
                s = e;
            }
        }
        let n = fetched.len() as u64;
        let m = self.mirrors.get_mut(&pane).expect("mirror");
        let t = m.transcript.get_or_insert_with(Default::default);
        t.extend(fetched);
        let mut lines: Vec<String> = t.values().cloned().collect();
        lines.extend(crate::mirror::trim_rows(&m.rows()));
        (lines, n)
    }

    /// Number of reconnects (new connections) after the first.
    pub fn reconnect_count(&self) -> usize {
        self.reconnects.len()
    }
}

// ---------------------------------------------------------------------------
// Raw connection (explicit resume tokens and input seqs)
// ---------------------------------------------------------------------------

/// A single connection driven by hand (what a restarted client process does
/// when it persisted its resume token).
#[derive(Debug)]
pub struct RawClient {
    /// Connection.
    pub conn: ClientConnection,
    /// Incoming.
    pub rx: ClientReceiver,
    /// Server hello.
    pub hello: ServerHello,
    /// Mirrors.
    pub mirrors: HashMap<PaneId, Mirror>,
    responses: HashMap<u64, Result<Response, RpcError>>,
    next_id: u64,
}

/// Hello for a raw client.
pub fn raw_hello(key: &SessionKey, client: ClientId) -> ClientHello {
    ClientHello::new(key.clone(), client, "civet-e2e-raw", "0")
}

impl RawClient {
    /// Wrap a connection.
    pub fn new(c: Connected) -> Self {
        Self {
            conn: c.conn,
            rx: c.incoming,
            hello: c.server_hello,
            mirrors: HashMap::new(),
            responses: HashMap::new(),
            next_id: 1000,
        }
    }

    /// Handle one message; false when the connection closed.
    pub async fn pump(&mut self, deadline: Instant) -> bool {
        let msg = match tokio::time::timeout_at(deadline, self.rx.recv()).await {
            Ok(Ok(Some(m))) => m,
            Ok(_) => return false,
            Err(_) => panic!("timed out waiting for the daemon"),
        };
        match msg {
            ServerMessage::Screen(u) => {
                self.mirrors
                    .entry(u.pane)
                    .or_insert_with(|| Mirror::new(true))
                    .apply(&u);
                let _ = self
                    .conn
                    .send(ClientMessage::Ack(ScreenAck {
                        pane: u.pane,
                        state: u.state,
                    }))
                    .await;
            }
            ServerMessage::Response(r) => {
                self.responses.insert(r.id, r.result);
            }
            _ => {}
        }
        true
    }

    /// RPC.
    pub async fn request(&mut self, request: Request) -> Result<Response, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        self.conn
            .send(ClientMessage::Request(ControlRequest { id, request }))
            .await
            .expect("send");
        self.wait_response(id).await
    }

    /// Wait for a response by id.
    pub async fn wait_response(&mut self, id: u64) -> Result<Response, RpcError> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(r) = self.responses.remove(&id) {
                return r;
            }
            assert!(self.pump(deadline).await, "connection closed");
        }
    }

    /// Wait until the pane's mirror satisfies `pred`.
    pub async fn wait_screen(&mut self, pane: PaneId, mut pred: impl FnMut(&Mirror) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !self.mirrors.get(&pane).is_some_and(&mut pred) {
            assert!(self.pump(deadline).await, "connection closed");
        }
    }

    /// Fetch `[start, end)` of history (QUIC scrollback stream).
    pub async fn fetch(&self, pane: PaneId, start: u64, end: u64) -> Vec<String> {
        let page = self
            .conn
            .fetch_scrollback(ScrollbackRequest {
                req_id: 1,
                pane,
                query: ScrollbackQuery::Range { start, end },
            })
            .await
            .expect("transport")
            .expect("rpc");
        page.lines.iter().map(|l| strip_vt(&l.vt)).collect()
    }
}
