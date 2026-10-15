//! `civet-cli`: command-line test client for `civetd` (milestone 1).
//!
//! Connects like the desktop app does (bootstrap over SSH or locally, then
//! QUIC with an SSH stdio-tunnel fallback) through a resumable
//! `ClientSession`, and either runs one control command (JSON output) or
//! attaches a pane to the current terminal. See `docs/INTERFACES.md` 5.4.
//!
//! ```text
//! civet-cli --local spawn --attach            # new shell pane, attached
//! civet-cli connect myhost                    # same over ssh + QUIC
//! civet-cli connect myhost --fallback         # SSH stdio tunnel only
//! civet-cli --ssh myhost panes                # JSON list
//! civet-cli --local --rebind-every 2 attach 1 # prove migration
//! ```

use std::io::{IsTerminal, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use civet_proto::{
    AgentStatus, BootstrapInfo, CommandSpec, Event, KillSignal, LaunchAgentRequest, PaneId, RepoId,
    Request, Response, ScrollbackPage, ScrollbackQuery, SpawnPaneRequest, TermSize, WorktreeId,
};
use civet_transport::{
    ClientSession, ClientSessionConfig, Dialer, FailoverConfig, FailoverDialer, LinkState,
    QuicClient, QuicClientConfig, QuicDialer, SessionEvent, StreamDialer, TransportError,
};
use civetd::bootstrap::{
    self, BinarySource, BootstrapOptions, BootstrapStep, SshTarget, open_local_tunnel,
    open_ssh_tunnel,
};
use clap::{Args, Parser, Subcommand};
use tokio::sync::mpsc;

/// Detach key in raw mode: Ctrl-].
const DETACH_KEY: u8 = 0x1d;
/// Without a tty: after stdin EOF, detach once the pane was quiet this long
/// (a pane that exits ends the attach right away).
const EOF_QUIET: Duration = Duration::from_secs(2);
/// QUIC connect timeout before falling back to the SSH tunnel.
const QUIC_FALLBACK_AFTER: Duration = Duration::from_secs(3);

#[derive(Debug, Parser)]
#[command(name = "civet-cli", version, about = "Civet test client")]
struct Cli {
    #[command(flatten)]
    conn: ConnectArgs,
    /// Client id (hex); default: a fresh random id per run. Two clients
    /// sharing an id kick each other off the daemon (one live connection
    /// per id), so only pass one to resume a specific client's state.
    #[arg(long, global = true)]
    client_id: Option<String>,
    /// Call notify_network_change (UDP rebind) every SECS seconds while
    /// attached and print the resume latency to stderr.
    #[arg(long, global = true, value_name = "SECS")]
    rebind_every: Option<f64>,
    /// Verbose logging / progress on stderr.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Args, Clone)]
struct ConnectArgs {
    /// Bootstrap over ssh to DEST (host, user@host or ssh_config alias).
    #[arg(long, global = true, value_name = "DEST")]
    ssh: Option<String>,
    /// ssh port.
    #[arg(long, global = true)]
    ssh_port: Option<u16>,
    /// Extra ssh argument (repeatable), e.g. --ssh-arg=-J --ssh-arg=bastion.
    #[arg(long = "ssh-arg", global = true, allow_hyphen_values = true)]
    ssh_args: Vec<String>,
    /// ssh program (a wrapper script works too).
    #[arg(long, global = true, env = "CIVET_SSH", default_value = "ssh")]
    ssh_program: PathBuf,
    /// Directory with civetd-<triple> binaries to upload.
    #[arg(long, global = true)]
    dist: Option<PathBuf>,
    /// Replace a running incompatible / busy daemon.
    #[arg(long, global = true)]
    replace: bool,
    /// Use a daemon on this machine (default when nothing else is given).
    #[arg(long, global = true)]
    local: bool,
    /// civetd binary for --local (default: next to civet-cli, else PATH).
    #[arg(long, global = true)]
    civetd: Option<PathBuf>,
    /// State dir of the local daemon (--local).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    /// Dial a printed bootstrap line directly (with --host).
    #[arg(long, global = true, value_name = "JSON")]
    bootstrap_line: Option<String>,
    /// Address for --bootstrap-line.
    #[arg(long, global = true)]
    host: Option<IpAddr>,
    /// Skip QUIC and use the stdio tunnel (ssh … civetd stdio-tunnel, or the
    /// local `civetd stdio-tunnel`): the UDP-blocked fallback.
    #[arg(long, global = true, visible_alias = "fallback")]
    stdio: bool,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Connect to DEST (ssh) or the local daemon and attach a pane: a new
    /// one running CMD (default: login shell) unless --pane is given.
    Connect {
        /// ssh destination; omit for --local.
        dest: Option<String>,
        /// Attach this existing pane instead of spawning one.
        #[arg(long)]
        pane: Option<u64>,
        /// Working directory for a new pane.
        #[arg(long)]
        cwd: Option<String>,
        /// Command for a new pane.
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// RTT per ping, then min/avg/max.
    Ping {
        #[arg(long, default_value_t = 5)]
        count: u32,
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
    },
    /// DaemonInfo.
    Info,
    /// Registered repos.
    Repos,
    /// Register a repo.
    AddRepo { path: String },
    /// Unregister a repo.
    RmRepo { repo: String },
    /// Worktrees of a repo.
    Worktrees { repo: String },
    /// Create a worktree.
    NewWorktree {
        repo: String,
        branch: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        path: Option<String>,
    },
    /// Remove a worktree.
    RmWorktree {
        worktree: String,
        #[arg(long)]
        force: bool,
    },
    /// List panes.
    Panes,
    /// Spawn a pane (login shell, or CMD after --).
    Spawn {
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        /// K=V (repeatable).
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Attach after spawning.
        #[arg(long)]
        attach: bool,
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// New worktree + agent pane.
    Launch {
        repo: String,
        branch: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        attach: bool,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Attach a pane to this terminal (Ctrl-] detaches).
    Attach { pane: u64 },
    /// Kill a pane.
    Kill {
        pane: u64,
        #[arg(long, default_value = "hup")]
        signal: String,
    },
    /// Resize a pane.
    Resize { pane: u64, cols: u16, rows: u16 },
    /// Print scrollback (JSON page, or raw VT lines with --raw).
    Scrollback {
        pane: u64,
        /// Lines before this absolute line (default: newest).
        #[arg(long)]
        before: Option<u64>,
        #[arg(long, default_value_t = 200)]
        limit: u32,
        /// Every retained line, oldest first (pages through the history).
        #[arg(long)]
        all: bool,
        /// VT lines instead of JSON.
        #[arg(long)]
        raw: bool,
    },
    /// SetAgentStatus.
    Status { pane: u64, status: String },
    /// Print events as JSON lines until Ctrl-C.
    Events,
}

fn print_json<T: serde::Serialize>(v: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// A connected session plus what it took to get there.
struct Conn {
    session: ClientSession,
    events: mpsc::UnboundedReceiver<SessionEvent>,
}

fn progress_printer(verbose: bool) -> impl Fn(BootstrapStep) + Send + Sync {
    move |step| {
        if verbose {
            eprintln!("civet-cli: bootstrap {step:?}");
        }
    }
}

fn local_civetd(args: &ConnectArgs) -> PathBuf {
    if let Some(p) = &args.civetd {
        return p.clone();
    }
    match bootstrap::sibling_civetd() {
        Ok(p) if p.is_file() => p,
        _ => PathBuf::from("civetd"),
    }
}

async fn quic_dialer(addr: SocketAddr, info: &BootstrapInfo) -> anyhow::Result<Arc<dyn Dialer>> {
    let client = Arc::new(QuicClient::new(addr.is_ipv6())?);
    let mut config = QuicClientConfig::new(addr, info.cert_fingerprint);
    config.connect_timeout = QUIC_FALLBACK_AFTER;
    Ok(Arc::new(QuicDialer { client, config }))
}

async fn connect(cli: &Cli, conn: &ConnectArgs) -> anyhow::Result<Conn> {
    let client_id = match &cli.client_id {
        Some(h) => civet_proto::ClientId::from_hex(h)?,
        None => civet_proto::ClientId::generate()?,
    };
    let session_for = |info: &BootstrapInfo| {
        ClientSessionConfig::new(
            info.session_key.clone(),
            client_id,
            "civet-cli",
            civetd::VERSION,
        )
    };

    if let Some(line) = &conn.bootstrap_line {
        let info = BootstrapInfo::from_json_line(line)?;
        let host = conn.host.context("--bootstrap-line needs --host")?;
        let dialer = quic_dialer(SocketAddr::new(host, info.udp_port), &info).await?;
        let (session, events) = ClientSession::connect(session_for(&info), dialer).await?;
        return Ok(Conn { session, events });
    }

    if let Some(dest) = &conn.ssh {
        let mut opts = BootstrapOptions::new(SshTarget {
            destination: dest.clone(),
            port: conn.ssh_port,
            extra_args: conn.ssh_args.clone(),
        });
        if let Some(d) = &conn.dist {
            opts.binary = BinarySource::Dir(d.clone());
        }
        opts.replace = conn.replace;
        opts.ssh_program = conn.ssh_program.clone();
        let progress = progress_printer(cli.verbose);
        let b = bootstrap::bootstrap_ssh(&opts, &progress).await?;
        if cli.verbose {
            eprintln!(
                "civet-cli: civetd {} at {} (installed: {}, reattached: {})",
                b.info.daemon_version, b.dial_addr, b.installed, b.info.reattached
            );
        }
        let tunnel = {
            let opts = opts.clone();
            let remote = b.remote_binary.clone();
            move || {
                let opts = opts.clone();
                let remote = remote.clone();
                async move {
                    open_ssh_tunnel(&opts, &remote)
                        .await
                        .map_err(std::io::Error::other)
                }
            }
        };
        let tunnel: Arc<dyn Dialer> = Arc::new(StreamDialer { factory: tunnel });
        let dialer: Arc<dyn Dialer> = if conn.stdio {
            tunnel
        } else {
            // QUIC, failing over to the SSH tunnel when UDP is (or becomes)
            // blocked, and moving back once QUIC works again.
            let quic = quic_dialer(b.dial_addr, &b.info).await?;
            Arc::new(FailoverDialer::new(quic, tunnel, FailoverConfig::default()))
        };
        let (session, events) = ClientSession::connect(session_for(&b.info), dialer).await?;
        if cli.verbose
            && let LinkState::Connected { transport, .. } = &*session.link_state().borrow()
        {
            eprintln!("civet-cli: connected over {transport:?}");
        }
        return Ok(Conn { session, events });
    }

    // Local (default).
    let civetd = local_civetd(conn);
    let b =
        bootstrap::bootstrap_local_with(&civetd, conn.state_dir.as_deref(), conn.replace).await?;
    if cli.verbose {
        eprintln!(
            "civet-cli: local civetd {} (udp {}, reattached: {})",
            b.info.daemon_version, b.info.udp_port, b.info.reattached
        );
    }
    let dialer: Arc<dyn Dialer> = if conn.stdio {
        let state_dir = conn.state_dir.clone();
        Arc::new(StreamDialer {
            factory: move || {
                let civetd = civetd.clone();
                let state_dir = state_dir.clone();
                async move {
                    open_local_tunnel(&civetd, state_dir.as_deref())
                        .await
                        .map_err(std::io::Error::other)
                }
            },
        })
    } else {
        quic_dialer(b.dial_addr, &b.info).await?
    };
    let (session, events) = ClientSession::connect(session_for(&b.info), dialer).await?;
    Ok(Conn { session, events })
}

async fn rpc(session: &ClientSession, req: Request) -> anyhow::Result<Response> {
    Ok(session.request(req).await?)
}

fn local_size() -> TermSize {
    if std::io::stdout().is_terminal()
        && let Ok((cols, rows)) = crossterm::terminal::size()
        && cols > 0
        && rows > 0
    {
        return TermSize::new(cols, rows);
    }
    TermSize::new(80, 24)
}

fn command_spec(cmd: &[String]) -> Option<CommandSpec> {
    let (program, args) = cmd.split_first()?;
    Some(CommandSpec::new(program.clone(), args.iter().cloned()))
}

fn parse_env(env: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    env.iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .with_context(|| format!("--env expects K=V, got {kv:?}"))
        })
        .collect()
}

async fn spawn_pane(
    session: &ClientSession,
    req: SpawnPaneRequest,
) -> anyhow::Result<civet_proto::PaneInfo> {
    match rpc(session, Request::SpawnPane(req)).await? {
        Response::PaneSpawned(info) => Ok(info),
        other => bail!("unexpected response {other:?}"),
    }
}

/// Restores the terminal on drop (also on panic / early return).
struct RawGuard {
    active: bool,
}

impl RawGuard {
    fn enter(tty: bool) -> anyhow::Result<Self> {
        if tty {
            crossterm::terminal::enable_raw_mode()?;
            crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
        }
        Ok(Self { active: tty })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.active {
            let _ =
                crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[derive(Debug, Default)]
struct AttachStats {
    reconnects: u32,
    resumes: u32,
    longest_recovering: Duration,
    bytes_in: u64,
    bytes_out: u64,
    rebinds: u32,
    rebind_latencies_ms: Vec<f64>,
}

/// Attach `pane` to this terminal until Ctrl-], pane exit, or (without a
/// tty) stdin EOF followed by [`EOF_QUIET`] without output.
async fn attach(
    conn: &mut Conn,
    pane: PaneId,
    rebind_every: Option<f64>,
    verbose: bool,
) -> anyhow::Result<()> {
    let session = conn.session.clone();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let guard = RawGuard::enter(tty)?;
    if tty {
        let size = local_size();
        let _ = rpc(&session, Request::ResizePane { pane, size }).await;
    }
    session.subscribe(pane).await?;

    // stdin → channel (blocking reads on their own thread).
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Option<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = in_tx.send(None);
                    return;
                }
                Ok(n) => {
                    if in_tx.send(Some(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut rebind = rebind_every
        .filter(|s| *s > 0.0)
        .map(|s| tokio::time::interval(Duration::from_secs_f64(s)));
    if let Some(r) = rebind.as_mut() {
        r.reset(); // first tick after one period
    }
    let (lat_tx, mut lat_rx) = mpsc::unbounded_channel::<f64>();

    let mut stats = AttachStats::default();
    let mut recovering_since: Option<Instant> = None;
    let mut stdin_eof_at: Option<Instant> = None;
    let mut last_output = Instant::now();
    let mut shown_state = 0u64;
    let mut stdout = std::io::stdout();
    let mut end_message: Option<String> = None;
    let mut seen_initial_connect = false;

    loop {
        let quiet_deadline = stdin_eof_at
            .map(|eof| tokio::time::Instant::from_std(last_output.max(eof) + EOF_QUIET));
        tokio::select! {
            ev = conn.events.recv() => {
                let Some(ev) = ev else {
                    end_message = Some("session ended".into());
                    break;
                };
                match ev {
                    SessionEvent::Screen(u) if u.pane == pane => {
                        stats.bytes_in += u.payload.len() as u64;
                        stdout.write_all(&u.payload)?;
                        stdout.flush()?;
                        shown_state = u.state;
                        last_output = Instant::now();
                    }
                    SessionEvent::Event(Event::PaneExited { pane: p, exit_code }) if p == pane => {
                        end_message = Some(format!("[process exited: {exit_code:?}]"));
                        // Events and screen updates travel on different
                        // streams: wait for the pane's final state.
                        let final_state = match session.request(Request::ListPanes).await {
                            Ok(Response::Panes(ps)) => ps.iter().find(|p| p.id == pane).map(|p| p.state),
                            _ => None,
                        };
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                        while final_state.is_some_and(|f| shown_state < f) {
                            match tokio::time::timeout_at(deadline, conn.events.recv()).await {
                                Ok(Some(SessionEvent::Screen(u))) if u.pane == pane => {
                                    shown_state = u.state;
                                    stats.bytes_in += u.payload.len() as u64;
                                    stdout.write_all(&u.payload)?;
                                }
                                Ok(Some(_)) => {}
                                Ok(None) | Err(_) => break,
                            }
                        }
                        stdout.flush()?;
                        break;
                    }
                    SessionEvent::Event(Event::PaneClosed { pane: p }) if p == pane => {
                        end_message = Some("[pane closed]".into());
                        break;
                    }
                    SessionEvent::Link(LinkState::Recovering { reason, .. }) => {
                        recovering_since.get_or_insert_with(Instant::now);
                        if verbose {
                            eprint!("civet-cli: recovering ({reason})\r\n");
                        }
                    }
                    SessionEvent::Link(LinkState::Connected { .. }) => {
                        if let Some(t) = recovering_since.take() {
                            stats.longest_recovering = stats.longest_recovering.max(t.elapsed());
                        }
                    }
                    SessionEvent::Link(LinkState::Failed { reason, .. }) => {
                        end_message = Some(format!("[connection failed: {reason}]"));
                        break;
                    }
                    SessionEvent::Connected { resumed, instance_changed, .. } => {
                        // The first one is the initial connect.
                        if !std::mem::replace(&mut seen_initial_connect, true) {
                            continue;
                        }
                        stats.reconnects += 1;
                        if resumed {
                            stats.resumes += 1;
                        }
                        if instance_changed {
                            end_message = Some("[daemon restarted; pane is gone]".into());
                            break;
                        }
                    }
                    SessionEvent::Goodbye { reason } => {
                        end_message = Some(format!("[daemon said goodbye: {reason}]"));
                        break;
                    }
                    _ => {}
                }
            }
            data = in_rx.recv() => match data.flatten() {
                Some(mut bytes) => {
                    if tty && let Some(i) = bytes.iter().position(|b| *b == DETACH_KEY) {
                        bytes.truncate(i);
                        if !bytes.is_empty() {
                            stats.bytes_out += bytes.len() as u64;
                            session.send_input(pane, bytes);
                        }
                        end_message = Some("[detached]".into());
                        break;
                    }
                    stats.bytes_out += bytes.len() as u64;
                    session.send_input(pane, bytes);
                }
                None => {
                    if tty {
                        end_message = Some("[stdin closed]".into());
                        break;
                    }
                    stdin_eof_at.get_or_insert_with(Instant::now);
                    // Park the channel; the quiet timer ends the attach.
                    let (_tx, rx) = mpsc::unbounded_channel();
                    in_rx = rx;
                }
            },
            _ = winch.recv(), if tty => {
                let size = local_size();
                let s = session.clone();
                tokio::spawn(async move {
                    let _ = s.request(Request::ResizePane { pane, size }).await;
                });
            }
            _ = async { rebind.as_mut().expect("guarded").tick().await }, if rebind.is_some() => {
                stats.rebinds += 1;
                let s = session.clone();
                let tx = lat_tx.clone();
                let t0 = Instant::now();
                s.notify_network_change();
                tokio::spawn(async move {
                    if s.request(Request::Ping { nonce: 0 }).await.is_ok() {
                        let _ = tx.send(t0.elapsed().as_secs_f64() * 1000.0);
                    }
                });
            }
            Some(ms) = lat_rx.recv() => {
                stats.rebind_latencies_ms.push(ms);
                eprint!("civet-cli: rebind #{}: resume latency {ms:.1} ms\r\n", stats.rebind_latencies_ms.len());
            }
            _ = async {
                match quiet_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                break;
            }
        }
    }
    drop(guard);
    if let Some(m) = end_message {
        eprintln!("\r\ncivet-cli: {m}");
    }
    let max_lat = stats
        .rebind_latencies_ms
        .iter()
        .copied()
        .fold(0.0f64, f64::max);
    eprintln!(
        "civet-cli: reconnects {} (resumed {}), longest recovering {} ms, bytes in {} / out {}{}",
        stats.reconnects,
        stats.resumes,
        stats.longest_recovering.as_millis(),
        stats.bytes_in,
        stats.bytes_out,
        if stats.rebinds > 0 {
            format!(
                ", rebinds {} (max resume latency {max_lat:.1} ms)",
                stats.rebinds
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

fn page_json(page: &ScrollbackPage) -> serde_json::Value {
    serde_json::json!({
        "start": page.start,
        "oldestAvailable": page.oldest_available,
        "endAvailable": page.end_available,
        "lines": page.lines.iter().map(|l| String::from_utf8_lossy(&l.vt).into_owned()).collect::<Vec<_>>(),
        "wrapped": page.lines.iter().map(|l| l.wrapped).collect::<Vec<_>>(),
    })
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let mut conn_args = cli.conn.clone();
    if let Cmd::Connect { dest: Some(d), .. } = &cli.command {
        conn_args.ssh = Some(d.clone());
    }
    let mut conn = connect(&cli, &conn_args).await?;
    let session = conn.session.clone();
    let result = run_command(&cli, &mut conn).await;
    session.close().await;
    result
}

async fn run_command(cli: &Cli, conn: &mut Conn) -> anyhow::Result<()> {
    let s = conn.session.clone();
    match &cli.command {
        Cmd::Connect { pane, cwd, cmd, .. } => {
            let pane = match pane {
                Some(p) => PaneId(*p),
                None => {
                    let info = spawn_pane(
                        &s,
                        SpawnPaneRequest {
                            worktree: None,
                            cwd: cwd.clone(),
                            command: command_spec(cmd),
                            env: Vec::new(),
                            size: local_size(),
                            title: None,
                            agent: None,
                        },
                    )
                    .await?;
                    eprintln!("civet-cli: spawned pane {}", info.id);
                    info.id
                }
            };
            attach(conn, pane, cli.rebind_every, cli.verbose).await
        }
        Cmd::Ping { count, interval_ms } => {
            let mut rtts = Vec::new();
            for i in 0..*count {
                let t0 = Instant::now();
                match rpc(
                    &s,
                    Request::Ping {
                        nonce: u64::from(i),
                    },
                )
                .await?
                {
                    Response::Pong { nonce, .. } if nonce == u64::from(i) => {}
                    other => bail!("unexpected ping reply {other:?}"),
                }
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                println!("ping {i}: {ms:.2} ms");
                rtts.push(ms);
                if i + 1 < *count {
                    tokio::time::sleep(Duration::from_millis(*interval_ms)).await;
                }
            }
            if !rtts.is_empty() {
                let min = rtts.iter().copied().fold(f64::INFINITY, f64::min);
                let max = rtts.iter().copied().fold(0.0, f64::max);
                let avg = rtts.iter().sum::<f64>() / rtts.len() as f64;
                println!("min/avg/max = {min:.2}/{avg:.2}/{max:.2} ms");
            }
            Ok(())
        }
        Cmd::Info => match rpc(&s, Request::GetDaemonInfo).await? {
            Response::DaemonInfo(i) => print_json(&i),
            other => bail!("unexpected response {other:?}"),
        },
        Cmd::Repos => match rpc(&s, Request::ListRepos).await? {
            Response::Repos(r) => print_json(&r),
            other => bail!("unexpected response {other:?}"),
        },
        Cmd::AddRepo { path } => {
            let path = std::path::absolute(path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.clone());
            match rpc(&s, Request::AddRepo { path }).await? {
                Response::Repo(r) => print_json(&r),
                other => bail!("unexpected response {other:?}"),
            }
        }
        Cmd::RmRepo { repo } => {
            rpc(
                &s,
                Request::RemoveRepo {
                    repo: RepoId(repo.clone()),
                },
            )
            .await?;
            print_json(&serde_json::json!({"ok": true}))
        }
        Cmd::Worktrees { repo } => {
            match rpc(
                &s,
                Request::ListWorktrees {
                    repo: RepoId(repo.clone()),
                },
            )
            .await?
            {
                Response::Worktrees(w) => print_json(&w),
                other => bail!("unexpected response {other:?}"),
            }
        }
        Cmd::NewWorktree {
            repo,
            branch,
            base,
            path,
        } => {
            let req = Request::CreateWorktree {
                repo: RepoId(repo.clone()),
                branch: branch.clone(),
                base: base.clone(),
                path: path.clone(),
            };
            match rpc(&s, req).await? {
                Response::Worktree(w) => print_json(&w),
                other => bail!("unexpected response {other:?}"),
            }
        }
        Cmd::RmWorktree { worktree, force } => {
            rpc(
                &s,
                Request::RemoveWorktree {
                    worktree: WorktreeId(worktree.clone()),
                    force: *force,
                },
            )
            .await?;
            print_json(&serde_json::json!({"ok": true}))
        }
        Cmd::Panes => match rpc(&s, Request::ListPanes).await? {
            Response::Panes(p) => print_json(&p),
            other => bail!("unexpected response {other:?}"),
        },
        Cmd::Spawn {
            worktree,
            cwd,
            env,
            agent,
            title,
            attach: do_attach,
            cmd,
        } => {
            let info = spawn_pane(
                &s,
                SpawnPaneRequest {
                    worktree: worktree.clone().map(WorktreeId),
                    cwd: cwd.clone(),
                    command: command_spec(cmd),
                    env: parse_env(env)?,
                    size: local_size(),
                    title: title.clone(),
                    agent: agent.clone(),
                },
            )
            .await?;
            if *do_attach {
                eprintln!("civet-cli: spawned pane {}", info.id);
                attach(conn, info.id, cli.rebind_every, cli.verbose).await
            } else {
                print_json(&info)
            }
        }
        Cmd::Launch {
            repo,
            branch,
            base,
            agent,
            attach: do_attach,
            cmd,
        } => {
            let command = command_spec(cmd).context("launch needs a command after --")?;
            let req = LaunchAgentRequest {
                repo: RepoId(repo.clone()),
                branch: branch.clone(),
                base: base.clone(),
                worktree_path: None,
                command,
                env: Vec::new(),
                size: local_size(),
                title: None,
                agent: agent.clone(),
            };
            match rpc(&s, Request::LaunchAgent(req)).await? {
                Response::AgentLaunched { worktree, pane } => {
                    if *do_attach {
                        eprintln!("civet-cli: worktree {} pane {}", worktree.path, pane.id);
                        attach(conn, pane.id, cli.rebind_every, cli.verbose).await
                    } else {
                        print_json(&serde_json::json!({"worktree": worktree, "pane": pane}))
                    }
                }
                other => bail!("unexpected response {other:?}"),
            }
        }
        Cmd::Attach { pane } => attach(conn, PaneId(*pane), cli.rebind_every, cli.verbose).await,
        Cmd::Kill { pane, signal } => {
            let signal = match signal.as_str() {
                "hup" | "hangup" => KillSignal::Hangup,
                "term" | "terminate" => KillSignal::Terminate,
                "kill" => KillSignal::Kill,
                other => bail!("unknown signal {other:?} (hup|term|kill)"),
            };
            rpc(
                &s,
                Request::KillPane {
                    pane: PaneId(*pane),
                    signal,
                },
            )
            .await?;
            print_json(&serde_json::json!({"ok": true}))
        }
        Cmd::Resize { pane, cols, rows } => {
            rpc(
                &s,
                Request::ResizePane {
                    pane: PaneId(*pane),
                    size: TermSize::new(*cols, *rows),
                },
            )
            .await?;
            print_json(&serde_json::json!({"ok": true}))
        }
        Cmd::Scrollback {
            pane,
            before,
            limit,
            all,
            raw,
        } => {
            let pane = PaneId(*pane);
            let pages = if *all {
                // Page upwards from the newest line, then print oldest first.
                let mut pages = Vec::new();
                let mut end = None;
                loop {
                    let page = s
                        .fetch_scrollback(
                            pane,
                            ScrollbackQuery::Before {
                                end,
                                limit: (*limit).max(1),
                            },
                        )
                        .await?;
                    let more = page.has_older() && !page.lines.is_empty();
                    end = Some(page.start);
                    pages.push(page);
                    if !more {
                        break;
                    }
                }
                pages.reverse();
                pages
            } else {
                vec![
                    s.fetch_scrollback(
                        pane,
                        ScrollbackQuery::Before {
                            end: *before,
                            limit: *limit,
                        },
                    )
                    .await?,
                ]
            };
            if *raw {
                let mut out = std::io::stdout().lock();
                for page in &pages {
                    for line in &page.lines {
                        out.write_all(&line.vt)?;
                        out.write_all(if line.wrapped { b"" } else { b"\r\n" })?;
                    }
                }
                out.flush()?;
                Ok(())
            } else if pages.len() == 1 {
                print_json(&page_json(&pages[0]))
            } else {
                print_json(&pages.iter().map(page_json).collect::<Vec<_>>())
            }
        }
        Cmd::Status { pane, status } => {
            let status: AgentStatus = status
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown status {status:?}"))?;
            rpc(
                &s,
                Request::SetAgentStatus {
                    pane: PaneId(*pane),
                    status,
                },
            )
            .await?;
            print_json(&serde_json::json!({"ok": true}))
        }
        Cmd::Events => {
            let mut rebind = cli
                .rebind_every
                .filter(|s| *s > 0.0)
                .map(|s| tokio::time::interval(Duration::from_secs_f64(s)));
            loop {
                tokio::select! {
                    ev = conn.events.recv() => match ev {
                        Some(SessionEvent::Event(e)) => println!("{}", serde_json::to_string(&e)?),
                        Some(SessionEvent::Goodbye { reason }) => {
                            eprintln!("civet-cli: daemon said goodbye: {reason}");
                            return Ok(());
                        }
                        Some(SessionEvent::Link(LinkState::Failed { reason, .. })) => bail!("connection failed: {reason}"),
                        Some(_) => {}
                        None => return Ok(()),
                    },
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                    _ = async { rebind.as_mut().expect("guarded").tick().await }, if rebind.is_some() => {
                        let t0 = Instant::now();
                        s.notify_network_change();
                        if s.request(Request::Ping { nonce: 0 }).await.is_ok() {
                            eprintln!("civet-cli: rebind: resume latency {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let filter = std::env::var("CIVET_LOG").ok().unwrap_or_else(|| {
        if cli.verbose {
            "info".into()
        } else {
            "warn".into()
        }
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("civet-cli: {e}");
            std::process::exit(1);
        }
    };
    let r = rt.block_on(run(cli));
    let code = match r {
        Ok(()) => 0,
        Err(e) => {
            if let Some(TransportError::Rejected(r)) = e.downcast_ref::<TransportError>() {
                eprintln!(
                    "civet-cli: daemon rejected us: {:?} {}",
                    r.reason, r.message
                );
            } else {
                eprintln!("civet-cli: {e:#}");
            }
            1
        }
    };
    // The stdin reader thread may be blocked in read(2): don't wait for it.
    std::process::exit(code);
}
