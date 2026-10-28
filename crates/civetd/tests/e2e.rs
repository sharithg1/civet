//! End-to-end tests on loopback: an in-process daemon (temp state dir, never
//! ~/.civet) and real clients (`ClientSession`, raw connections, the real
//! `civetd` binary for `stdio-tunnel` and `bootstrap`).

#![cfg(feature = "server")]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use civet_proto::{
    BootstrapInfo, ClientHello, ClientId, ClientMessage, CommandSpec, ControlRequest, Event,
    InputMessage, PaneId, Request, Response, RpcError, ScreenAck, ScreenUpdate, ScrollbackQuery,
    ScrollbackRequest, ServerHello, ServerMessage, SpawnPaneRequest, TermSize, UpdateKind,
};
use civet_term::{PaneTerminal, TermConfig};
use civet_transport::{
    ClientConnection, ClientReceiver, ClientSession, ClientSessionConfig, Connected, Dialer,
    LinkState, QuicClient, QuicClientConfig, QuicDialer, SessionEvent, StreamDialer, TransportKind,
};
use civetd::daemon::{Daemon, DaemonOptions, ExitError};
use tokio::sync::mpsc::UnboundedReceiver;

const CIVETD: &str = env!("CARGO_BIN_EXE_civetd");
const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn opts(dir: &Path) -> DaemonOptions {
    let mut o = DaemonOptions::new(dir);
    o.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    o.port_range = 0..=0;
    o.shell = Some("/bin/sh".into());
    o
}

async fn start_daemon() -> (Daemon, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let d = Daemon::start(opts(dir.path())).await.unwrap();
    (d, dir)
}

fn sh_pane(cwd: &Path) -> Request {
    Request::SpawnPane(SpawnPaneRequest {
        worktree: None,
        cwd: Some(cwd.display().to_string()),
        command: Some(CommandSpec::new("sh", Vec::<String>::new())),
        env: vec![("PS1".into(), "$ ".into())],
        size: TermSize::new(80, 24),
        title: None,
        agent: None,
    })
}

/// How often a command printed `word`: its occurrences in the screen text.
/// Why not whole rows: input typed ahead is echoed by the tty, on Linux in
/// pieces and asynchronously, so an echo can share a row with the output
/// (`echo t''wdup`). Commands quote their word (`echo d''up`) so that only
/// the output spells it.
fn count_output(rows: &[String], word: &str) -> usize {
    rows.join("\n").matches(word).count()
}

/// Strip CSI / OSC escape sequences from a VT line.
fn strip_vt(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Client-side copy of a pane, fed with the daemon's payloads (as xterm.js
/// would be). Diffs must apply to exactly their base state.
struct Mirror {
    term: PaneTerminal,
    state: u64,
    fulls: usize,
    diffs: usize,
    last_kind: Option<UpdateKind>,
}

impl Mirror {
    fn new() -> Self {
        Self {
            term: PaneTerminal::new(TermConfig::default()),
            state: 0,
            fulls: 0,
            diffs: 0,
            last_kind: None,
        }
    }

    fn apply(&mut self, u: &ScreenUpdate) {
        if u.kind == UpdateKind::Diff {
            assert_eq!(u.base_state, self.state, "diff applied to the wrong base");
            self.diffs += 1;
        } else {
            self.fulls += 1;
        }
        if u.size != self.term.size() {
            self.term.resize(u.size).unwrap();
        }
        self.term.feed(&u.payload);
        self.state = u.state;
        self.last_kind = Some(u.kind);
    }

    fn rows(&self) -> Vec<String> {
        self.term.screen_text()
    }

    fn has_row(&self, row: &str) -> bool {
        self.rows().iter().any(|r| r.trim_end() == row)
    }

    /// The shell's `$ ` prompt is on screen. Why: typing before the shell
    /// is reading leaves the tty's echo of the command above the prompt and
    /// the command's output on the prompt row (`$ hello`), which a row match
    /// never sees; dash on Linux starts slowly enough to hit this. Matches `$` (PS1) and
    /// macOS's `sh-3.2$` (bash 3.2 as sh ignores PS1 from the environment).
    fn has_prompt(&self) -> bool {
        self.rows().iter().any(|r| r.trim_end().ends_with('$'))
    }

    /// See [`count_output`].
    fn count_output(&self, word: &str) -> usize {
        count_output(&self.rows(), word)
    }

    /// History (as replayed into this mirror's scrollback) + screen, as text.
    fn all_lines(&self) -> Vec<String> {
        let store = self.term.scrollback();
        let mut v: Vec<String> = store
            .range(store.oldest(), store.end())
            .map(|(_, l)| strip_vt(&l.vt))
            .collect();
        v.extend(self.rows());
        v
    }
}

/// Assert `lines` contains "1".."=n" as consecutive lines, in order.
fn assert_contiguous_numbers(lines: &[String], n: u32) {
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "1")
        .unwrap_or_else(|| panic!("no line '1' in {lines:?}"));
    for i in 1..=n {
        let got = lines.get(start + i as usize - 1).map(|l| l.trim_end());
        assert_eq!(
            got,
            Some(i.to_string().as_str()),
            "line for {i} (lines: {lines:?})"
        );
    }
}

/// Waits for session events, applying screen updates to per-pane mirrors.
async fn wait_session(
    events: &mut UnboundedReceiver<SessionEvent>,
    mirrors: &mut HashMap<PaneId, Mirror>,
    mut done: impl FnMut(&HashMap<PaneId, Mirror>, Option<&SessionEvent>) -> bool,
) {
    let deadline = tokio::time::Instant::now() + WAIT;
    if done(mirrors, None) {
        return;
    }
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("timed out waiting for the session")
            .expect("session ended");
        if let SessionEvent::Screen(u) = &ev {
            mirrors.entry(u.pane).or_insert_with(Mirror::new).apply(u);
        }
        if done(mirrors, Some(&ev)) {
            return;
        }
    }
}

async fn session_over(
    dialer: Arc<dyn Dialer>,
    info: &BootstrapInfo,
) -> (ClientSession, UnboundedReceiver<SessionEvent>) {
    let cfg = ClientSessionConfig::new(
        info.session_key.clone(),
        ClientId::generate().unwrap(),
        "e2e",
        "0",
    );
    ClientSession::connect(cfg, dialer).await.unwrap()
}

async fn spawn_sh(session: &ClientSession, cwd: &Path) -> PaneId {
    match session.request(sh_pane(cwd)).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("unexpected {other:?}"),
    }
}

/// A connection driven by hand (resume tokens, explicit input seqs).
struct Raw {
    conn: ClientConnection,
    rx: ClientReceiver,
    hello: ServerHello,
    mirrors: HashMap<PaneId, Mirror>,
    responses: HashMap<u64, Result<Response, RpcError>>,
    input_acks: Vec<(PaneId, u64)>,
    goodbye: Option<String>,
    events: Vec<Event>,
    next_id: u64,
}

impl Raw {
    fn new(c: Connected) -> Self {
        Self {
            conn: c.conn,
            rx: c.incoming,
            hello: c.server_hello,
            mirrors: HashMap::new(),
            responses: HashMap::new(),
            input_acks: Vec::new(),
            goodbye: None,
            events: Vec::new(),
            next_id: 100,
        }
    }

    /// Handle one incoming message (screen updates are applied and acked).
    async fn pump(&mut self, deadline: tokio::time::Instant) -> bool {
        let msg = match tokio::time::timeout_at(deadline, self.rx.recv()).await {
            Ok(Ok(Some(m))) => m,
            Ok(_) => return false,
            Err(_) => panic!("timed out waiting for the daemon"),
        };
        match msg {
            ServerMessage::Screen(u) => {
                self.mirrors
                    .entry(u.pane)
                    .or_insert_with(Mirror::new)
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
            ServerMessage::InputAck { pane, seq } => self.input_acks.push((pane, seq)),
            ServerMessage::Goodbye { reason } => self.goodbye = Some(reason),
            ServerMessage::Event(e) => self.events.push(e),
        }
        true
    }

    async fn request(&mut self, request: Request) -> Result<Response, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        self.conn
            .send(ClientMessage::Request(ControlRequest { id, request }))
            .await
            .unwrap();
        self.wait_response(id).await
    }

    async fn wait_response(&mut self, id: u64) -> Result<Response, RpcError> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(r) = self.responses.remove(&id) {
                return r;
            }
            assert!(self.pump(deadline).await, "connection closed");
        }
    }

    async fn input(&self, pane: PaneId, seq: u64, data: &str) {
        self.conn
            .send(ClientMessage::Input(InputMessage {
                pane,
                seq,
                data: data.as_bytes().to_vec(),
            }))
            .await
            .unwrap();
    }

    async fn wait_screen(&mut self, pane: PaneId, mut pred: impl FnMut(&Mirror) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if self.mirrors.get(&pane).is_some_and(&mut pred) {
                return;
            }
            assert!(self.pump(deadline).await, "connection closed");
        }
    }
}

/// A raw connection over the daemon's unix socket that says it is client
/// `version` (decides which newer events it gets).
async fn raw(daemon: &Daemon, info: &BootstrapInfo, version: &str) -> Raw {
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let hello = ClientHello::new(
        info.session_key.clone(),
        ClientId::generate().unwrap(),
        "e2e-raw",
        version,
    );
    Raw::new(
        civet_transport::connect_stream(sock, hello, vec![])
            .await
            .unwrap(),
    )
}

fn hello(info: &BootstrapInfo, client: ClientId) -> ClientHello {
    ClientHello::new(info.session_key.clone(), client, "e2e-raw", "0")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// QUIC end to end: spawn `sh`, `echo hello`, see it on screen; then rebind
/// the client's UDP socket (network change) and check the next keystroke
/// is echoed within 200 ms, several times in a row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quic_echo_and_rebind_resume_latency() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let client = Arc::new(QuicClient::new(false).unwrap());
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client: client.clone(),
        config: QuicClientConfig::new(
            SocketAddr::from(([127, 0, 0, 1], info.udp_port)),
            info.cert_fingerprint,
        ),
    });
    let (session, mut events) = session_over(dialer, &info).await;
    assert!(matches!(
        *session.link_state().borrow(),
        LinkState::Connected {
            transport: TransportKind::Quic,
            ..
        }
    ));
    let pane = spawn_sh(&session, dir.path()).await;
    session.subscribe(pane).await.unwrap();
    let mut mirrors = HashMap::new();
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(Mirror::has_prompt)
    })
    .await;
    session.send_input(pane, b"echo hello\n".to_vec());
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(|m| m.has_row("hello"))
    })
    .await;

    let mut latencies = Vec::new();
    for i in 0..5 {
        let before = client.local_addr().unwrap();
        let t0 = Instant::now();
        session.notify_network_change();
        let word = format!("again{i}");
        session.send_input(pane, format!("echo {word}\n").into_bytes());
        wait_session(&mut events, &mut mirrors, |m, _| {
            m.get(&pane).is_some_and(|m| m.has_row(&word))
        })
        .await;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        latencies.push(ms);
        assert_ne!(
            client.local_addr().unwrap(),
            before,
            "rebind must move to a new UDP socket"
        );
    }
    let max = latencies.iter().copied().fold(0.0, f64::max);
    eprintln!("rebind -> keystroke echoed: {latencies:.1?} ms (max {max:.1})");
    assert!(max < 200.0, "resume latency {max} ms");
    // Migration, not a reconnect: still the first connection, never resumed.
    while let Ok(ev) = events.try_recv() {
        assert!(
            !matches!(ev, SessionEvent::Connected { .. }),
            "rebind caused a reconnect"
        );
    }
    session.close().await;
    daemon.shutdown("test done").await;
}

/// Drop the client entirely (no Goodbye), let the pane produce output while
/// nobody is connected, reconnect with the resume token: the subscription
/// resumes with a Diff from the last acked state that replays everything
/// into the client's scrollback, and scrollback paging has it too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_disconnect_loses_nothing() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let client = QuicClient::new(false).unwrap();
    let cfg = QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint);
    let cid = ClientId::generate().unwrap();

    let mut c1 = Raw::new(
        client
            .connect(&cfg, hello(&info, cid), vec![])
            .await
            .unwrap(),
    );
    assert!(!c1.hello.resumed);
    let pane = match c1.request(sh_pane(dir.path())).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    c1.request(Request::SubscribePane {
        pane,
        known_state: None,
    })
    .await
    .unwrap();
    c1.wait_screen(pane, Mirror::has_prompt).await;
    c1.input(pane, 1, "echo start\n").await;
    c1.wait_screen(pane, |m| m.has_row("start")).await;
    c1.input(pane, 2, "sleep 1; seq 1 300; echo done\n").await;
    // Make sure the command reached the shell (its echo is on screen).
    c1.wait_screen(pane, |m| m.rows().iter().any(|r| r.contains("seq 1 300")))
        .await;
    let token = c1.hello.resume_token.clone();
    let known = c1.mirrors[&pane].state;
    assert!(!c1.mirrors[&pane].has_row("done"));
    let mut old_mirror = c1.mirrors.remove(&pane).unwrap();
    drop(c1); // no Goodbye: the daemon keeps our resume state

    // Output happens while disconnected.
    let deadline = Instant::now() + WAIT;
    while !daemon
        .sessions()
        .screen_text(pane)
        .unwrap()
        .iter()
        .any(|r| r == "done")
    {
        assert!(Instant::now() < deadline, "pane never finished");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let t0 = Instant::now();
    let resub = ClientMessage::Request(ControlRequest {
        id: 1,
        request: Request::SubscribePane {
            pane,
            known_state: Some(known),
        },
    });
    let mut c2 = Raw::new(
        client
            .connect(
                &cfg,
                hello(&info, cid).with_resume(Some(token)),
                vec![resub],
            )
            .await
            .unwrap(),
    );
    assert!(c2.hello.resumed, "resume token must be accepted");
    assert_eq!(c2.hello.resumed_subscriptions.len(), 1);
    assert_eq!(c2.hello.resumed_subscriptions[0].pane, pane);
    assert_eq!(c2.hello.resumed_subscriptions[0].last_input_seq, 2);

    // Continue from the old client's screen (state `known`): the first
    // update must be a Diff whose base is exactly that state, and applying
    // the updates must bring the old screen up to date, with everything that
    // scrolled off replayed into its scrollback.
    let mut first: Option<ScreenUpdate> = None;
    let deadline = tokio::time::Instant::now() + WAIT;
    while first.is_none() {
        match tokio::time::timeout_at(deadline, c2.rx.recv()).await {
            Ok(Ok(Some(ServerMessage::Screen(u)))) => first = Some(u),
            Ok(Ok(Some(ServerMessage::Response(r)))) => {
                c2.responses.insert(r.id, r.result);
            }
            Ok(Ok(Some(_))) => {}
            other => panic!("no screen update after resume: {other:?}"),
        }
    }
    let first = first.unwrap();
    let resume_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "reconnect + resume -> first update: {resume_ms:.1} ms ({} B payload, history {}..{})",
        first.payload.len(),
        first.history_start,
        first.history_lines
    );
    assert_eq!(first.kind, UpdateKind::Diff);
    assert_eq!(first.base_state, known);
    old_mirror.apply(&first);
    c2.mirrors.insert(pane, old_mirror);
    assert!(matches!(
        c2.wait_response(1).await.unwrap(),
        Response::Subscribed(_)
    ));
    c2.wait_screen(pane, |m| m.has_row("done")).await;
    assert_eq!(c2.mirrors[&pane].fulls, 1, "only the initial Full");
    assert_contiguous_numbers(&c2.mirrors[&pane].all_lines(), 300);

    // Scrollback paging has every line that scrolled off, contiguous.
    let page = c2
        .conn
        .fetch_scrollback(ScrollbackRequest {
            req_id: 7,
            pane,
            query: ScrollbackQuery::Range {
                start: 0,
                end: first.history_lines,
            },
        })
        .await
        .unwrap()
        .unwrap();
    let mut lines: Vec<String> = page.lines.iter().map(|l| strip_vt(&l.vt)).collect();
    lines.extend(daemon.sessions().screen_text(pane).unwrap());
    assert_contiguous_numbers(&lines, 300);
    assert!(lines.iter().any(|l| l == "done"));
    c2.conn.close().await;
    daemon.shutdown("test done").await;
}

/// Same scenario through `ClientSession` (what the desktop uses): kill the
/// connection under it; it resumes on its own and the mirror (fed only with
/// the session's updates) ends up with every line, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_resume_replays_history_into_client() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    // A stream dialer over the unix socket that we can cut at will.
    let socket = daemon.socket_path();
    let (cut_tx, cut_rx) = tokio::sync::watch::channel(0u32);
    let cut_rx = Arc::new(cut_rx);
    let dialer: Arc<dyn Dialer> = Arc::new(StreamDialer {
        factory: move || {
            let socket = socket.clone();
            let mut cut = (*cut_rx).clone();
            async move {
                let s = tokio::net::UnixStream::connect(&socket).await?;
                let (ours, theirs) = tokio::io::duplex(1 << 16);
                // Proxy so the test can kill the link without a Goodbye.
                tokio::spawn(async move {
                    let (mut a, mut b) = (s, theirs);
                    cut.borrow_and_update();
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                        _ = cut.changed() => {}
                    }
                });
                Ok(ours)
            }
        },
    });
    let (session, mut events) = session_over(dialer, &info).await;
    let pane = spawn_sh(&session, dir.path()).await;
    session.subscribe(pane).await.unwrap();
    let mut mirrors = HashMap::new();
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(Mirror::has_prompt)
    })
    .await;
    session.send_input(pane, b"echo ready\n".to_vec());
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(|m| m.has_row("ready"))
    })
    .await;
    session.send_input(pane, b"sleep 1; seq 1 500; echo finished\n".to_vec());
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane)
            .is_some_and(|m| m.rows().iter().any(|r| r.contains("seq 1 500")))
    })
    .await;
    cut_tx.send(1).unwrap(); // link dies silently
    let mut resumed = false;
    wait_session(&mut events, &mut mirrors, |m, ev| {
        if let Some(SessionEvent::Connected { resumed: r, .. }) = ev {
            resumed = *r;
        }
        m.get(&pane).is_some_and(|m| m.has_row("finished"))
    })
    .await;
    assert!(resumed, "the session must resume, not start fresh");
    let m = &mirrors[&pane];
    assert_eq!(m.fulls, 1, "resume must not need a Full repaint");
    assert_contiguous_numbers(&m.all_lines(), 500);
    session.close().await;
    daemon.shutdown("test done").await;
}

/// The same input seq is applied once, also when resent after a resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_input_seq_not_applied_twice() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let cid = ClientId::generate().unwrap();
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let mut c = Raw::new(
        civet_transport::connect_stream(sock, hello(&info, cid), vec![])
            .await
            .unwrap(),
    );
    let pane = match c.request(sh_pane(dir.path())).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    c.request(Request::SubscribePane {
        pane,
        known_state: None,
    })
    .await
    .unwrap();
    c.wait_screen(pane, Mirror::has_prompt).await;
    c.input(pane, 1, "echo d''up\n").await;
    c.input(pane, 1, "echo d''up\n").await;
    c.input(pane, 2, "echo t''wo\n").await;
    c.wait_screen(pane, |m| m.count_output("two") > 0).await;
    // Let any (wrongly) duplicated write show up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    while tokio::time::timeout_at(deadline, c.pump(deadline + WAIT))
        .await
        .is_ok()
    {}
    let m = &c.mirrors[&pane];
    assert_eq!(m.count_output("dup"), 1, "{:#?}", m.rows());
    assert!(
        c.input_acks.iter().any(|(p, s)| *p == pane && *s >= 1),
        "a duplicate is answered with an InputAck"
    );
    assert_eq!(daemon.sessions().last_input_seq(cid, pane), 2);

    // Resume over a new connection and resend seq 2 (as a client does with
    // unacked input) plus a new seq 3.
    let token = c.hello.resume_token.clone();
    let known = c.mirrors[&pane].state;
    drop(c);
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let resub = ClientMessage::Request(ControlRequest {
        id: 1,
        request: Request::SubscribePane {
            pane,
            known_state: Some(known),
        },
    });
    let connected = civet_transport::connect_stream(
        sock,
        hello(&info, cid).with_resume(Some(token)),
        vec![resub],
    )
    .await
    .unwrap();
    let mut c2 = Raw::new(connected);
    assert!(c2.hello.resumed);
    assert_eq!(c2.hello.resumed_subscriptions[0].last_input_seq, 2);
    c2.input(pane, 2, "echo t''wo\n").await;
    c2.input(pane, 3, "echo th''ree\n").await;
    c2.wait_response(1).await.unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = daemon.sessions().screen_text(pane).unwrap();
        if count_output(&rows, "three") > 0 {
            assert_eq!(count_output(&rows, "two"), 1, "{rows:#?}");
            assert_eq!(count_output(&rows, "dup"), 1, "{rows:#?}");
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    c2.conn.close().await;
    daemon.shutdown("test done").await;
}

/// Ping/Pong over both transports, and GetDaemonInfo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ping_pong() {
    let (daemon, _dir) = start_daemon().await;
    let info = daemon.info().clone();
    let client = QuicClient::new(false).unwrap();
    let cfg = QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint);
    let mut q = Raw::new(
        client
            .connect(&cfg, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    match q.request(Request::Ping { nonce: 42 }).await.unwrap() {
        Response::Pong {
            nonce,
            server_time_ms,
        } => {
            assert_eq!(nonce, 42);
            assert!(server_time_ms > 0);
        }
        other => panic!("{other:?}"),
    }
    match q.request(Request::GetDaemonInfo).await.unwrap() {
        Response::DaemonInfo(d) => {
            assert_eq!(d.instance_id, q.hello.instance_id);
            assert_eq!(d.pid, std::process::id());
        }
        other => panic!("{other:?}"),
    }
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let mut s = Raw::new(
        civet_transport::connect_stream(sock, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    assert!(matches!(
        s.request(Request::Ping { nonce: 7 }).await.unwrap(),
        Response::Pong { nonce: 7, .. }
    ));
    q.conn.close().await;
    s.conn.close().await;
    daemon.shutdown("test done").await;
}

/// File ops over the wire: create, write, chunked read, trash + restore
/// (into the daemon's state dir), and the error codes clients key on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_ops_round_trip() {
    use civet_proto::{ErrorCode, FileOp};
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let repo = dir.path().join("proj");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hi\n").unwrap();
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let mut c = Raw::new(
        civet_transport::connect_stream(sock, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    let Response::Repo(r) = c
        .request(Request::AddRepo {
            path: repo.display().to_string(),
        })
        .await
        .unwrap()
    else {
        panic!("AddRepo")
    };
    let Response::Worktrees(w) = c
        .request(Request::ListWorktrees { repo: r.id })
        .await
        .unwrap()
    else {
        panic!("ListWorktrees")
    };
    let worktree = w[0].id.clone();
    let op = |op: FileOp| Request::FileOp {
        worktree: worktree.clone(),
        op,
    };

    let data: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
    let resp = c
        .request(op(FileOp::Write {
            path: "blob.bin".into(),
            data: data.clone(),
        }))
        .await
        .unwrap();
    assert!(
        matches!(resp, Response::FileOpDone(ref r) if r.path == "blob.bin"),
        "{resp:?}"
    );
    let Response::FileChunk(chunk) = c
        .request(Request::ReadFileChunk {
            worktree: worktree.clone(),
            path: "blob.bin".into(),
            offset: 60_000,
            len: 1 << 20,
        })
        .await
        .unwrap()
    else {
        panic!("ReadFileChunk")
    };
    assert_eq!(
        (chunk.size, chunk.data.as_slice()),
        (100_000, &data[60_000..])
    );

    let err = c
        .request(op(FileOp::CreateFile {
            path: "README".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::AlreadyExists);
    let err = c
        .request(op(FileOp::CreateFile {
            path: ".git/hooks/x".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    let Response::FileOpDone(t) = c
        .request(op(FileOp::Trash {
            path: "README".into(),
        }))
        .await
        .unwrap()
    else {
        panic!("Trash")
    };
    assert!(!repo.join("README").exists());
    let id = t.trash_id.unwrap();
    assert!(dir.path().join("trash").join(&id).join("item").is_file());
    c.request(op(FileOp::Restore { trash_id: id }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("README")).unwrap(),
        "hi\n"
    );
    c.conn.close().await;
    daemon.shutdown("test done").await;
}

/// Folder projects over the wire: AddRepo still refuses a plain folder,
/// AddProject registers it, ListRepos (old clients) leaves it out, its one
/// worktree browses files, and git / panes in it are Unsupported.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_projects_over_the_control_stream() {
    use civet_proto::{ErrorCode, ProjectKind, SpawnPaneRequest, TermSize};
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let folder = dir.path().join("notes");
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    std::fs::write(folder.join("docs/todo.md"), "- ship\n").unwrap();
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let mut c = Raw::new(
        civet_transport::connect_stream(sock, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    let path = folder.display().to_string();
    let err = c
        .request(Request::AddRepo { path: path.clone() })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("not a git repository"), "{err:?}");
    let Response::Project(p) = c.request(Request::AddProject { path }).await.unwrap() else {
        panic!("AddProject")
    };
    assert_eq!(p.kind, ProjectKind::Folder);
    assert_eq!(p.repo.name, "notes");
    let Response::Repos(repos) = c.request(Request::ListRepos).await.unwrap() else {
        panic!("ListRepos")
    };
    assert!(repos.is_empty());
    let Response::Projects(projects) = c.request(Request::ListProjects).await.unwrap() else {
        panic!("ListProjects")
    };
    assert_eq!(projects, vec![p.clone()]);
    let Response::Worktrees(w) = c
        .request(Request::ListWorktrees {
            repo: p.repo.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("ListWorktrees")
    };
    assert_eq!(w.len(), 1);
    let worktree = w[0].id.clone();
    let Response::AllFiles(files) = c
        .request(Request::ListAllFiles {
            worktree: worktree.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("ListAllFiles")
    };
    assert_eq!(files.paths, ["docs/todo.md"]);
    let Response::FilePreview(preview) = c
        .request(Request::ReadFile {
            worktree: worktree.clone(),
            path: "docs/todo.md".into(),
            max_bytes: 1024,
        })
        .await
        .unwrap()
    else {
        panic!("ReadFile")
    };
    assert_eq!(preview.text, "- ship\n");
    let err = c
        .request(Request::GitStatus {
            worktree: worktree.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
    let Response::PaneSpawned(pane) = c
        .request(Request::SpawnPane(SpawnPaneRequest {
            worktree: Some(worktree.clone()),
            cwd: None,
            command: None,
            env: vec![],
            size: TermSize::new(80, 24),
            title: None,
            agent: None,
        }))
        .await
        .unwrap()
    else {
        panic!("SpawnPane")
    };
    assert_eq!(pane.worktree, Some(worktree));
    c.conn.close().await;
    daemon.shutdown("test done").await;
}

/// Graceful shutdown: every client (QUIC session and stream link) gets
/// `Goodbye` before the connection closes; daemon.json and the socket are
/// removed and the lock is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graceful_shutdown_sends_goodbye() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    // Second daemon on the same state dir is refused (exit code 3).
    let err = Daemon::start(opts(dir.path())).await.unwrap_err();
    assert_eq!(err.downcast_ref::<ExitError>().map(|e| e.code), Some(3));

    let client = Arc::new(QuicClient::new(false).unwrap());
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client,
        config: QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint),
    });
    let (session, mut events) = session_over(dialer, &info).await;
    let pane = spawn_sh(&session, dir.path()).await;
    session.subscribe(pane).await.unwrap();
    let sock = tokio::net::UnixStream::connect(daemon.socket_path())
        .await
        .unwrap();
    let mut raw = Raw::new(
        civet_transport::connect_stream(sock, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    raw.request(Request::Ping { nonce: 1 }).await.unwrap();

    let paths = civetd::paths::StatePaths::new(dir.path());
    assert!(paths.daemon_json().exists());
    daemon.shutdown("upgrade in progress").await;

    let mut got = None;
    let mut mirrors = HashMap::new();
    wait_session(&mut events, &mut mirrors, |_, ev| {
        if let Some(SessionEvent::Goodbye { reason }) = ev {
            got = Some(reason.clone());
        }
        got.is_some()
    })
    .await;
    assert_eq!(got.as_deref(), Some("upgrade in progress"));
    let deadline = tokio::time::Instant::now() + WAIT;
    while raw.goodbye.is_none() && raw.pump(deadline).await {}
    assert_eq!(raw.goodbye.as_deref(), Some("upgrade in progress"));

    assert!(!paths.daemon_json().exists());
    assert!(!paths.socket().exists());
    // Lock released: a new daemon can start in the same dir.
    let again = Daemon::start(opts(dir.path())).await.unwrap();
    assert_ne!(again.info().session_key, info.session_key);
    again.shutdown("test done").await;
    let _ = events;
}

/// The real `civetd stdio-tunnel` binary pipes a ClientSession to a daemon's
/// unix socket (the UDP-blocked fallback path); exit code 2 without daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stdio_tunnel_binary() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
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
    let (session, mut events) = session_over(dialer, &info).await;
    assert!(matches!(
        *session.link_state().borrow(),
        LinkState::Connected {
            transport: TransportKind::Stream,
            ..
        }
    ));
    assert!(matches!(
        session.request(Request::Ping { nonce: 3 }).await.unwrap(),
        Response::Pong { nonce: 3, .. }
    ));
    let pane = spawn_sh(&session, dir.path()).await;
    session.subscribe(pane).await.unwrap();
    let mut mirrors = HashMap::new();
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(Mirror::has_prompt)
    })
    .await;
    session.send_input(pane, b"seq 1 40; echo hello-tunnel\n".to_vec());
    wait_session(&mut events, &mut mirrors, |m, _| {
        m.get(&pane).is_some_and(|m| m.has_row("hello-tunnel"))
    })
    .await;
    let page = session
        .fetch_scrollback(
            pane,
            ScrollbackQuery::Before {
                end: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert!(!page.lines.is_empty(), "scrollback over the tunnel");
    let mut lines: Vec<String> = page.lines.iter().map(|l| strip_vt(&l.vt)).collect();
    lines.extend(mirrors[&pane].rows());
    assert_contiguous_numbers(&lines, 40);
    session.close().await;
    daemon.shutdown("test done").await;

    // No daemon: exit code 2.
    let empty = tempfile::tempdir().unwrap();
    let status = tokio::process::Command::new(CIVETD)
        .arg("stdio-tunnel")
        .arg("--state-dir")
        .arg(empty.path())
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

/// `civetd bootstrap` via the client's local mode: starts a fully detached
/// daemon (the call returns although its stdout is a pipe we read to EOF),
/// prints one JSON line, reattaches on the second call; QUIC works; SIGTERM
/// makes the daemon say Goodbye and clean up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bootstrap_local_mode() {
    /// Stops the detached daemon even if an assertion fails.
    struct StopOnDrop(std::path::PathBuf);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            let json = civetd::paths::StatePaths::new(&self.0).daemon_json();
            if let Some(info) = std::fs::read_to_string(json)
                .ok()
                .and_then(|s| BootstrapInfo::from_json_line(&s).ok())
            {
                let _ = std::process::Command::new("kill")
                    .arg(info.pid.to_string())
                    .status();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let _stop = StopOnDrop(dir.path().to_path_buf());
    let civetd = Path::new(CIVETD);
    let t0 = Instant::now();
    let b = tokio::time::timeout(
        Duration::from_secs(15),
        civetd::bootstrap::bootstrap_local(civetd, Some(dir.path())),
    )
    .await
    .expect("bootstrap must not hang on inherited pipes")
    .unwrap();
    eprintln!("bootstrap (cold start) took {:?}", t0.elapsed());
    assert!(!b.info.reattached);
    assert_ne!(b.info.pid, std::process::id());
    assert_eq!(b.info.daemon_version, civetd::VERSION);
    assert_eq!(b.dial_addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));

    // The raw output really is a single JSON line.
    let out = tokio::process::Command::new(civetd)
        .arg("bootstrap")
        .arg("--state-dir")
        .arg(dir.path())
        .output()
        .await
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "stdout: {stdout:?}");
    let again = BootstrapInfo::from_json_line(&stdout).unwrap();
    assert!(again.reattached);
    assert_eq!(again.pid, b.info.pid);
    assert_eq!(again.session_key, b.info.session_key);

    let b2 = civetd::bootstrap::bootstrap_local(civetd, Some(dir.path()))
        .await
        .unwrap();
    assert!(b2.info.reattached);

    let client = Arc::new(QuicClient::new(false).unwrap());
    let dialer: Arc<dyn Dialer> = Arc::new(QuicDialer {
        client,
        config: QuicClientConfig::new(b.dial_addr, b.info.cert_fingerprint),
    });
    let (session, mut events) = session_over(dialer, &b.info).await;
    assert!(matches!(
        session.request(Request::Ping { nonce: 9 }).await.unwrap(),
        Response::Pong { nonce: 9, .. }
    ));

    let status = std::process::Command::new("kill")
        .arg(b.info.pid.to_string())
        .status()
        .unwrap();
    assert!(status.success());
    let mut mirrors = HashMap::new();
    let mut goodbye = false;
    wait_session(&mut events, &mut mirrors, |_, ev| {
        goodbye |= matches!(ev, Some(SessionEvent::Goodbye { .. }));
        goodbye
    })
    .await;
    let paths = civetd::paths::StatePaths::new(dir.path());
    let deadline = Instant::now() + WAIT;
    while paths.daemon_json().exists() {
        assert!(Instant::now() < deadline, "daemon.json not removed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = Event::ReposChanged; // keep the import set stable
}

/// OSC 52 from a pane reaches subscribed clients that are new enough as
/// `Event::Clipboard`, and nobody else; a re-subscribe (full repaint) does
/// not re-send it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn osc52_clipboard_event_reaches_subscribed_clients_only() {
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let mut a = raw(&daemon, &info, civetd::VERSION).await;
    let mut b = raw(&daemon, &info, civetd::VERSION).await; // not subscribed
    let mut old = raw(&daemon, &info, "0.1.0").await; // subscribed, too old
    let pane = match a.request(sh_pane(dir.path())).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    for c in [&mut a, &mut old] {
        let r = c
            .request(Request::SubscribePane {
                pane,
                known_state: None,
            })
            .await;
        assert!(matches!(r, Ok(Response::Subscribed(_))), "{r:?}");
    }
    a.wait_screen(pane, Mirror::has_prompt).await;
    // Query first (must be ignored), then a set: "hi" base64.
    a.input(pane, 1, "printf '\\033]52;c;?\\007\\033]52;c;aGk=\\007'\n")
        .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    let clip = |c: &Raw| {
        c.events
            .iter()
            .filter(|e| matches!(e, Event::Clipboard { .. }))
            .cloned()
            .collect::<Vec<_>>()
    };
    while clip(&a).is_empty() {
        assert!(a.pump(deadline).await, "connection closed");
    }
    assert_eq!(
        clip(&a),
        vec![Event::Clipboard {
            pane,
            text: "hi".into()
        }]
    );
    // Let the others drain whatever the daemon sent them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    for c in [&mut b, &mut old] {
        assert!(matches!(
            c.request(Request::Ping { nonce: 1 }).await,
            Ok(Response::Pong { .. })
        ));
        assert!(clip(c).is_empty(), "{:?}", c.events);
    }
    // Re-subscribing (full repaint) does not replay the clipboard write.
    assert!(matches!(
        a.request(Request::UnsubscribePane { pane }).await,
        Ok(Response::Ok)
    ));
    assert!(matches!(
        a.request(Request::SubscribePane {
            pane,
            known_state: None
        })
        .await,
        Ok(Response::Subscribed(_))
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(
        a.request(Request::Ping { nonce: 2 }).await,
        Ok(Response::Pong { .. })
    ));
    assert_eq!(clip(&a).len(), 1);
    for c in [a, b, old] {
        c.conn.close().await;
    }
    daemon.shutdown("test done").await;
}

/// Prompts over the wire: a queued prompt is listed and announced to
/// clients new enough for `Event::PromptQueue` (not to older ones), can be
/// cancelled, and `SendPrompt` types a command into a shell and runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_over_the_control_stream() {
    use civet_proto::{ErrorCode, PromptTrigger};
    let (daemon, dir) = start_daemon().await;
    let info = daemon.info().clone();
    let mut a = raw(&daemon, &info, civetd::VERSION).await;
    let mut old = raw(&daemon, &info, "0.4.1").await;
    let pane = match a.request(sh_pane(dir.path())).await.unwrap() {
        Response::PaneSpawned(p) => p.id,
        other => panic!("{other:?}"),
    };
    let queued = match a
        .request(Request::QueuePrompt {
            pane,
            text: "later".into(),
            trigger: PromptTrigger::Done,
        })
        .await
    {
        Ok(Response::QueuedPrompt(p)) => p,
        other => panic!("{other:?}"),
    };
    assert_eq!((queued.pane, queued.text.as_str()), (pane, "later"));
    let queue_events = |c: &Raw| {
        c.events
            .iter()
            .filter(|e| matches!(e, Event::PromptQueue { .. }))
            .cloned()
            .collect::<Vec<_>>()
    };
    let deadline = tokio::time::Instant::now() + WAIT;
    while queue_events(&a).is_empty() {
        assert!(a.pump(deadline).await, "connection closed");
    }
    assert_eq!(
        queue_events(&a),
        vec![Event::PromptQueue {
            pane,
            prompts: vec![queued.clone()],
            sent: None
        }]
    );
    assert_eq!(
        old.request(Request::ListQueuedPrompts).await,
        Ok(Response::QueuedPrompts(vec![queued.clone()]))
    );
    assert!(queue_events(&old).is_empty(), "{:?}", old.events);
    assert_eq!(
        a.request(Request::CancelQueuedPrompt { id: queued.id })
            .await,
        Ok(Response::Ok)
    );
    let err = a
        .request(Request::CancelQueuedPrompt { id: queued.id })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // `sh` has no bracketed paste: typed as is, then Enter runs it.
    let subscribed = a
        .request(Request::SubscribePane {
            pane,
            known_state: None,
        })
        .await;
    assert!(matches!(subscribed, Ok(Response::Subscribed(_))));
    a.wait_screen(pane, Mirror::has_prompt).await;
    assert_eq!(
        a.request(Request::SendPrompt {
            pane,
            text: "echo prompt-$((40 + 2))\n".into(),
        })
        .await,
        Ok(Response::Ok)
    );
    a.wait_screen(pane, |m| {
        m.rows().iter().any(|r| r.trim_end() == "prompt-42")
    })
    .await;
    let err = a
        .request(Request::SendPrompt {
            pane,
            text: "\n".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    for c in [a, old] {
        c.conn.close().await;
    }
    daemon.shutdown("test done").await;
}

/// Git actions over the wire: a discard comes back as a snapshot that
/// undoes it, the branch summary answers, and a path out of the worktree is
/// an `InvalidArgument` error, not a dropped connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_actions_over_the_control_stream() {
    use civet_proto::{ErrorCode, GitAction};
    let (daemon, dir) = start_daemon().await;
    let repo_dir = dir.path().join("proj");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo_dir.join("a.txt"), "a\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "a"]);
    std::fs::write(repo_dir.join("a.txt"), "edited\n").unwrap();

    let info = daemon.info().clone();
    let client = QuicClient::new(false).unwrap();
    let cfg = QuicClientConfig::new(daemon.quic_addr(), info.cert_fingerprint);
    let mut q = Raw::new(
        client
            .connect(&cfg, hello(&info, ClientId::generate().unwrap()), vec![])
            .await
            .unwrap(),
    );
    let repo = match q
        .request(Request::AddRepo {
            path: repo_dir.display().to_string(),
        })
        .await
        .unwrap()
    {
        Response::Repo(r) => r,
        other => panic!("{other:?}"),
    };
    let worktree = match q
        .request(Request::ListWorktrees { repo: repo.id })
        .await
        .unwrap()
    {
        Response::Worktrees(w) => w[0].id.clone(),
        other => panic!("{other:?}"),
    };
    match q
        .request(Request::GitBranch {
            worktree: worktree.clone(),
        })
        .await
        .unwrap()
    {
        Response::GitBranch(b) => assert_eq!(b.branch.as_deref(), Some("main")),
        other => panic!("{other:?}"),
    }
    let snapshot = match q
        .request(Request::GitAction {
            worktree: worktree.clone(),
            action: GitAction::Discard {
                paths: vec!["a.txt".into()],
                staged: false,
            },
        })
        .await
        .unwrap()
    {
        Response::GitDiscarded(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(std::fs::read(repo_dir.join("a.txt")).unwrap(), b"a\n");
    assert!(matches!(
        q.request(Request::GitAction {
            worktree: worktree.clone(),
            action: GitAction::RestoreDiscarded(snapshot),
        })
        .await
        .unwrap(),
        Response::Ok
    ));
    assert_eq!(std::fs::read(repo_dir.join("a.txt")).unwrap(), b"edited\n");
    let err = q
        .request(Request::GitAction {
            worktree,
            action: GitAction::Stage {
                paths: vec!["../escape".into()],
            },
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    q.conn.close().await;
    daemon.shutdown("test done").await;
}
