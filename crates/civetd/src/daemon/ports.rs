//! Listening-port detection (for `StreamOpen::Ports`) and the daemon end of
//! TCP port forwarding (`StreamOpen::Forward`).
//!
//! The scanner runs only while at least one client watches. Every
//! [`SCAN_INTERVAL`] it lists the TCP sockets in LISTEN state that belong
//! to the daemon's user and finds their processes: on Linux from
//! `/proc/net/tcp{,6}` and `/proc/<pid>/fd` (the fd walk only runs for
//! sockets it has not seen before), on macOS from `lsof` and `ps`. A port
//! belongs to a pane when its process descends from the pane's process, and
//! to a worktree through that pane or, failing that, through the process's
//! working directory. Watchers get the full list whenever it changes.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use civet_proto::{
    ErrorCode, ListeningPort, MAX_PORT_COMMAND_CHARS, PaneId, PortsUpdate, RpcError, WorktreeId,
};
use civet_session::SessionManager;
use civet_transport::{ForwardRequest, PortsSink};
use tokio::net::TcpStream;
use tokio::sync::watch;

/// Time between scans while someone watches.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// How long the daemon tries to connect to a forwarded port.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Parent links followed when attributing a process to a pane.
const MAX_ANCESTRY: usize = 64;

/// Shared listening-port state: the latest list and whether a scanner runs.
pub(crate) struct PortWatcher {
    tx: watch::Sender<Arc<PortsUpdate>>,
    scanning: AtomicBool,
    sessions: SessionManager,
}

impl std::fmt::Debug for PortWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortWatcher")
            .field("watchers", &self.tx.receiver_count())
            .finish()
    }
}

impl PortWatcher {
    pub(crate) fn new(sessions: SessionManager) -> Arc<Self> {
        Arc::new(Self {
            tx: watch::channel(Arc::new(PortsUpdate::default())).0,
            scanning: AtomicBool::new(false),
            sessions,
        })
    }

    /// Serve one `StreamOpen::Ports` stream until the client closes it or
    /// the connection ends.
    pub(crate) async fn serve(self: Arc<Self>, mut sink: PortsSink) {
        let mut rx = self.tx.subscribe();
        self.ensure_scanner();
        // A fresh scan first, so the client's first update is current
        // rather than whatever the last watcher saw.
        let first = Arc::new(scan_now(&self.sessions).await);
        self.tx
            .send_if_modified(|cur| replace_if_changed(cur, &first));
        rx.mark_unchanged();
        if sink.send(&first).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let update = rx.borrow_and_update().clone();
                    if sink.send(&update).await.is_err() {
                        return;
                    }
                }
                () = sink.stopped() => return,
            }
        }
    }

    fn ensure_scanner(self: &Arc<Self>) {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(scanner(weak));
    }
}

fn replace_if_changed(cur: &mut Arc<PortsUpdate>, new: &Arc<PortsUpdate>) -> bool {
    if **cur == **new {
        false
    } else {
        *cur = new.clone();
        true
    }
}

/// Scan every [`SCAN_INTERVAL`] while anyone watches, then stop.
async fn scanner(weak: Weak<PortWatcher>) {
    tracing::debug!("port scanner started");
    let mut cache = Cache::default();
    loop {
        tokio::time::sleep(SCAN_INTERVAL).await;
        let Some(w) = weak.upgrade() else { return };
        if w.tx.receiver_count() == 0 {
            w.scanning.store(false, Ordering::SeqCst);
            // A watcher that arrived between the check and the store
            // found `scanning` still set and did not start a scanner.
            if w.tx.receiver_count() == 0 || w.scanning.swap(true, Ordering::SeqCst) {
                tracing::debug!("port scanner stopped (no watchers)");
                return;
            }
        }
        let panes = w.sessions.pane_processes();
        let (update, c) = match tokio::task::spawn_blocking(move || {
            let u = scan(&panes, &mut cache);
            (u, cache)
        })
        .await
        {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "port scan panicked");
                w.scanning.store(false, Ordering::SeqCst);
                return;
            }
        };
        cache = c;
        let update = Arc::new(update);
        w.tx.send_if_modified(|cur| replace_if_changed(cur, &update));
    }
}

async fn scan_now(sessions: &SessionManager) -> PortsUpdate {
    let panes = sessions.pane_processes();
    tokio::task::spawn_blocking(move || scan(&panes, &mut Cache::default()))
        .await
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

/// Connect to `127.0.0.1:port` (then `[::1]:port`: dev servers often bind
/// only `localhost`, which may resolve to either) and pipe bytes both ways
/// until both sides are done.
pub(crate) async fn forward(req: ForwardRequest) {
    let port = req.port();
    if port == 0 {
        req.reject(RpcError::new(ErrorCode::InvalidArgument, "port 0"))
            .await;
        return;
    }
    let mut tcp = match connect_loopback(port).await {
        Ok(s) => s,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::ConnectionRefused {
                ErrorCode::NotFound
            } else {
                ErrorCode::Io
            };
            let message = if code == ErrorCode::NotFound {
                format!("nothing is listening on port {port}")
            } else {
                format!("cannot connect to port {port}: {e}")
            };
            tracing::debug!(port, error = %e, "forward refused");
            req.reject(RpcError::new(code, message)).await;
            return;
        }
    };
    let _ = tcp.set_nodelay(true);
    let mut tunnel = match req.accept().await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(port, error = %e, "forward stream gone before the reply");
            return;
        }
    };
    match tokio::io::copy_bidirectional(&mut tcp, &mut tunnel).await {
        Ok((up, down)) => tracing::debug!(port, up, down, "forwarded connection done"),
        Err(e) => tracing::debug!(port, error = %e, "forwarded connection ended"),
    }
}

async fn connect_loopback(port: u16) -> std::io::Result<TcpStream> {
    let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);
    let attempt = |addr| async move {
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                ))
            })
    };
    match attempt(v4).await {
        Ok(s) => Ok(s),
        Err(e4) => attempt(v6).await.map_err(|e6| {
            // Report "refused" only if neither family had a listener.
            if e4.kind() == std::io::ErrorKind::ConnectionRefused {
                e6
            } else {
                e4
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// One listening socket as the OS reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Socket {
    port: u16,
    addr: IpAddr,
    pid: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct ProcInfo {
    name: Option<String>,
    command: Option<String>,
    ppid: Option<u32>,
    cwd: Option<String>,
}

/// What the platform scanner keeps between scans.
#[derive(Debug, Default)]
struct Cache {
    /// Linux: socket inode -> owning pid (None: not found, retried later).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    owners: HashMap<u64, (Option<u32>, std::time::Instant)>,
}

/// How long an inode whose owner was not found is left alone before the
/// fd walk is tried again for it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const OWNER_RETRY: Duration = Duration::from_secs(30);

fn scan(panes: &[(PaneId, u32, Option<WorktreeId>)], cache: &mut Cache) -> PortsUpdate {
    let (sockets, mut procs) = platform::list(cache);
    build(&sockets, procs.as_mut(), panes)
}

/// Group sockets by port and attribute them to panes / worktrees.
fn build(
    sockets: &[Socket],
    procs: &mut dyn ProcSource,
    panes: &[(PaneId, u32, Option<WorktreeId>)],
) -> PortsUpdate {
    let pane_of_pid: HashMap<u32, (PaneId, Option<WorktreeId>)> = panes
        .iter()
        .map(|(id, pid, wt)| (*pid, (*id, wt.clone())))
        .collect();
    let mut worktrees: Vec<&WorktreeId> = panes.iter().filter_map(|(_, _, w)| w.as_ref()).collect();
    // Longest first, so a nested worktree wins over its parent directory.
    worktrees.sort_by_key(|w| std::cmp::Reverse(w.0.len()));
    worktrees.dedup();

    let mut by_port: BTreeMap<u16, ListeningPort> = BTreeMap::new();
    for s in sockets {
        let entry = by_port.entry(s.port).or_insert_with(|| ListeningPort {
            port: s.port,
            addrs: Vec::new(),
            pid: None,
            process: None,
            command: None,
            pane: None,
            worktree: None,
        });
        let addr = s.addr.to_string();
        if !entry.addrs.contains(&addr) {
            entry.addrs.push(addr);
        }
        if entry.pid.is_none()
            && let Some(pid) = s.pid
        {
            entry.pid = Some(pid);
            let info = procs.info(pid);
            entry.process = info.name.clone();
            entry.command = info.command.as_deref().map(truncate_command);
            if let Some((pane, wt)) = pane_ancestor(pid, procs, &pane_of_pid) {
                entry.pane = Some(pane);
                entry.worktree = wt;
            }
            if entry.worktree.is_none()
                && let Some(cwd) = info.cwd.as_deref()
            {
                entry.worktree = worktrees
                    .iter()
                    .find(|w| path_within(cwd, &w.0))
                    .map(|w| (*w).clone());
            }
        }
    }
    for p in by_port.values_mut() {
        p.addrs.sort();
    }
    PortsUpdate {
        ports: by_port.into_values().collect(),
    }
}

fn pane_ancestor(
    pid: u32,
    procs: &mut dyn ProcSource,
    panes: &HashMap<u32, (PaneId, Option<WorktreeId>)>,
) -> Option<(PaneId, Option<WorktreeId>)> {
    let mut cur = pid;
    for _ in 0..MAX_ANCESTRY {
        if let Some(found) = panes.get(&cur) {
            return Some(found.clone());
        }
        match procs.info(cur).ppid {
            Some(p) if p > 1 && p != cur => cur = p,
            _ => return None,
        }
    }
    None
}

fn path_within(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn truncate_command(c: &str) -> String {
    let c = c.trim();
    if c.chars().count() <= MAX_PORT_COMMAND_CHARS {
        return c.to_string();
    }
    let mut s: String = c.chars().take(MAX_PORT_COMMAND_CHARS - 1).collect();
    s.push('…');
    s
}

/// Process details, looked up lazily and memoized for one scan.
trait ProcSource {
    fn info(&mut self, pid: u32) -> ProcInfo;
}

#[cfg(any(test, target_os = "linux"))]
mod proc_net {
    //! `/proc/net/tcp` parsing (pure, so it is tested on every platform).

    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// TCP_LISTEN in `/proc/net/tcp`'s `st` column.
    const LISTEN: &str = "0A";

    /// (addr, port, uid, inode) of every LISTEN row.
    pub(super) fn parse(text: &str, v6: bool) -> Vec<(IpAddr, u16, u32, u64)> {
        let mut out = Vec::new();
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != LISTEN {
                continue;
            }
            let Some((addr, port)) = f[1].split_once(':') else {
                continue;
            };
            let (Some(addr), Ok(port), Ok(uid), Ok(inode)) = (
                parse_addr(addr, v6),
                u16::from_str_radix(port, 16),
                f[7].parse::<u32>(),
                f[9].parse::<u64>(),
            ) else {
                continue;
            };
            out.push((addr, port, uid, inode));
        }
        out
    }

    /// The kernel prints each 32-bit word of the (network-order) address
    /// with `%08X` in host byte order.
    fn parse_addr(hex: &str, v6: bool) -> Option<IpAddr> {
        let word = |i: usize| -> Option<[u8; 4]> {
            let w = u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok()?;
            Some(w.to_ne_bytes())
        };
        if v6 {
            if hex.len() != 32 {
                return None;
            }
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            let a = Ipv6Addr::from(b);
            Some(match a.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(a),
            })
        } else {
            if hex.len() != 8 {
                return None;
            }
            Some(IpAddr::V4(Ipv4Addr::from(word(0)?)))
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::path::Path;

    struct Procfs {
        memo: HashMap<u32, ProcInfo>,
    }

    impl ProcSource for Procfs {
        fn info(&mut self, pid: u32) -> ProcInfo {
            self.memo
                .entry(pid)
                .or_insert_with(|| read_proc(pid))
                .clone()
        }
    }

    fn read_proc(pid: u32) -> ProcInfo {
        let dir = Path::new("/proc").join(pid.to_string());
        let stat = std::fs::read_to_string(dir.join("stat")).ok();
        // `pid (comm) state ppid ...`; comm may contain spaces and parens.
        let (name, ppid) = match stat.as_deref().and_then(|s| {
            let open = s.find('(')?;
            let close = s.rfind(')')?;
            Some((s.get(open + 1..close)?, s.get(close + 1..)?))
        }) {
            Some((comm, rest)) => (
                Some(comm.to_string()),
                rest.split_whitespace().nth(1).and_then(|p| p.parse().ok()),
            ),
            None => (None, None),
        };
        let command = std::fs::read(dir.join("cmdline")).ok().and_then(|b| {
            let s: Vec<String> = b
                .split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            (!s.is_empty()).then(|| s.join(" "))
        });
        let cwd = std::fs::read_link(dir.join("cwd"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        ProcInfo {
            name,
            command,
            ppid,
            cwd,
        }
    }

    /// Find the owners of `wanted` socket inodes by walking `/proc/*/fd`
    /// (only processes we may inspect, i.e. our own).
    fn find_owners(wanted: &[u64]) -> HashMap<u64, u32> {
        let mut found = HashMap::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return found;
        };
        for p in procs.flatten() {
            let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let Some(inode) = target
                    .to_str()
                    .and_then(|t| t.strip_prefix("socket:["))
                    .and_then(|t| t.strip_suffix(']'))
                    .and_then(|t| t.parse::<u64>().ok())
                else {
                    continue;
                };
                if wanted.contains(&inode) {
                    found.entry(inode).or_insert(pid);
                    if found.len() == wanted.len() {
                        return found;
                    }
                }
            }
        }
        found
    }

    pub(super) fn list(cache: &mut Cache) -> (Vec<Socket>, Box<dyn ProcSource>) {
        // SAFETY: getuid has no preconditions and cannot fail.
        #[allow(unsafe_code)]
        let uid = unsafe { libc::getuid() };
        let mut rows = Vec::new();
        for (path, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
            if let Ok(text) = std::fs::read_to_string(path) {
                rows.extend(super::proc_net::parse(&text, v6));
            }
        }
        rows.retain(|r| r.2 == uid);

        let now = std::time::Instant::now();
        cache
            .owners
            .retain(|inode, _| rows.iter().any(|r| r.3 == *inode));
        let unknown: Vec<u64> = rows
            .iter()
            .map(|r| r.3)
            .filter(|i| match cache.owners.get(i) {
                None => true,
                Some((None, at)) => now.duration_since(*at) >= OWNER_RETRY,
                Some((Some(_), _)) => false,
            })
            .collect();
        if !unknown.is_empty() {
            let found = find_owners(&unknown);
            for i in unknown {
                cache.owners.insert(i, (found.get(&i).copied(), now));
            }
        }
        let sockets = rows
            .into_iter()
            .map(|(addr, port, _, inode)| Socket {
                port,
                addr,
                pid: cache.owners.get(&inode).and_then(|o| o.0),
            })
            .collect();
        (
            sockets,
            Box::new(Procfs {
                memo: HashMap::new(),
            }),
        )
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! `lsof` for the sockets, one `ps` for names, parents and commands.

    use super::*;

    struct PsTable {
        procs: HashMap<u32, ProcInfo>,
    }

    impl ProcSource for PsTable {
        fn info(&mut self, pid: u32) -> ProcInfo {
            self.procs.get(&pid).cloned().unwrap_or_default()
        }
    }

    fn run(cmd: &str, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        // lsof exits 1 when nothing matched.
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub(super) fn list(_cache: &mut Cache) -> (Vec<Socket>, Box<dyn ProcSource>) {
        // SAFETY: getuid has no preconditions and cannot fail.
        #[allow(unsafe_code)]
        let uid = unsafe { libc::getuid() }.to_string();
        let sockets = run(
            "/usr/sbin/lsof",
            &["-nP", "-a", "-u", &uid, "-iTCP", "-sTCP:LISTEN", "-Fpn"],
        )
        .map(|s| super::parse_lsof(&s))
        .unwrap_or_default();
        let procs = run("/bin/ps", &["-A", "-o", "pid=,ppid=,comm=,command="])
            .map(|s| parse_ps(&s))
            .unwrap_or_default();
        (sockets, Box::new(PsTable { procs }))
    }

    /// `pid ppid comm command...`; `comm` is a path on macOS, so only its
    /// file name is kept. (A comm with spaces is mis-split; it then only
    /// loses its name, the command line stays right.)
    fn parse_ps(text: &str) -> HashMap<u32, ProcInfo> {
        let mut out = HashMap::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(ppid), Some(comm)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
                continue;
            };
            let command: Vec<&str> = it.collect();
            out.insert(
                pid,
                ProcInfo {
                    name: comm.rsplit('/').next().map(str::to_string),
                    command: (!command.is_empty()).then(|| command.join(" ")),
                    ppid: Some(ppid),
                    cwd: None,
                },
            );
        }
        out
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    struct Nothing;

    impl ProcSource for Nothing {
        fn info(&mut self, _pid: u32) -> ProcInfo {
            ProcInfo::default()
        }
    }

    pub(super) fn list(_cache: &mut Cache) -> (Vec<Socket>, Box<dyn ProcSource>) {
        (Vec::new(), Box::new(Nothing))
    }
}

/// `lsof -F pn` output: `p<pid>` starts a process, `n<addr>:<port>` names
/// one of its sockets (`*:3000`, `127.0.0.1:5173`, `[::1]:8080`).
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn parse_lsof(text: &str) -> Vec<Socket> {
    let mut out = Vec::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse::<u32>().ok();
        } else if let Some(n) = line.strip_prefix('n') {
            let Some((addr, port)) = n.rsplit_once(':') else {
                continue;
            };
            let Ok(port) = port.parse::<u16>() else {
                continue;
            };
            let addr = addr.trim_start_matches('[').trim_end_matches(']');
            let addr = match addr {
                "*" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                a => match a.parse::<IpAddr>() {
                    Ok(a) => a,
                    Err(_) => continue,
                },
            };
            let s = Socket { port, addr, pid };
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_tcp_listen_rows() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41234 1 0000000000000000 100 0 0 10 0
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 999 1 0000000000000000 100 0 0 10 0
   2: 0100007F:1435 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 0 1 0000000000000000 20 4 30 10 -1
";
        let rows = proc_net::parse(v4, false);
        assert_eq!(rows.len(), 2);
        // 127.0.0.1:5173 when the host is little-endian (the kernel prints
        // host order); the bytes round-trip either way.
        if cfg!(target_endian = "little") {
            assert_eq!(
                rows[0],
                (IpAddr::V4(Ipv4Addr::LOCALHOST), 5173, 1000, 41234)
            );
            assert_eq!(rows[1].0, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        }
        assert_eq!(rows[1].1, 22);

        let v6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5555 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000000000000:0BB8 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 5556 1 0000000000000000 100 0 0 10 0
";
        let rows = proc_net::parse(v6, true);
        assert_eq!(rows.len(), 2);
        if cfg!(target_endian = "little") {
            assert_eq!(rows[0].0, IpAddr::V6(Ipv6Addr::LOCALHOST));
        }
        assert_eq!(rows[0].1, 8080);
        assert_eq!(rows[1].0, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        assert_eq!(rows[1].1, 3000);
    }

    #[test]
    fn lsof_output() {
        let out = "p501\nn*:3000\nn[::1]:5173\np777\nn127.0.0.1:5173\nnbogus\n";
        let s = parse_lsof(out);
        assert_eq!(
            s,
            vec![
                Socket {
                    port: 3000,
                    addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    pid: Some(501)
                },
                Socket {
                    port: 5173,
                    addr: IpAddr::V6(Ipv6Addr::LOCALHOST),
                    pid: Some(501)
                },
                Socket {
                    port: 5173,
                    addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    pid: Some(777)
                },
            ]
        );
    }

    struct Fake(HashMap<u32, ProcInfo>);

    impl ProcSource for Fake {
        fn info(&mut self, pid: u32) -> ProcInfo {
            self.0.get(&pid).cloned().unwrap_or_default()
        }
    }

    fn proc(name: &str, ppid: u32, cwd: &str) -> ProcInfo {
        ProcInfo {
            name: Some(name.into()),
            command: Some(format!("{name} --serve")),
            ppid: Some(ppid),
            cwd: Some(cwd.into()),
        }
    }

    #[test]
    fn attributes_ports_to_panes_and_worktrees() {
        let wt = WorktreeId("/src/app-worktrees/feat".into());
        let main = WorktreeId("/src/app".into());
        let panes = vec![
            (PaneId(3), 100, Some(wt.clone())),
            (PaneId(4), 200, Some(main.clone())),
        ];
        let mut procs = Fake(HashMap::from([
            (100, proc("zsh", 1, "/src/app-worktrees/feat")),
            (110, proc("npm", 100, "/src/app-worktrees/feat")),
            (120, proc("node", 110, "/src/app-worktrees/feat")),
            // Detached from any pane (reparented to init) but running in
            // the main worktree.
            (300, proc("python3", 1, "/src/app/api")),
            (400, proc("postgres", 1, "/var/lib/pg")),
        ]));
        let sockets = vec![
            Socket {
                port: 5173,
                addr: IpAddr::V6(Ipv6Addr::LOCALHOST),
                pid: Some(120),
            },
            Socket {
                port: 5173,
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                pid: Some(120),
            },
            Socket {
                port: 8000,
                addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                pid: Some(300),
            },
            Socket {
                port: 5432,
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                pid: Some(400),
            },
            Socket {
                port: 9999,
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                pid: None,
            },
        ];
        let u = build(&sockets, &mut procs, &panes);
        let ports: Vec<u16> = u.ports.iter().map(|p| p.port).collect();
        assert_eq!(ports, vec![5173, 5432, 8000, 9999]);
        let vite = &u.ports[0];
        assert_eq!(vite.addrs, vec!["127.0.0.1", "::1"]);
        assert_eq!(vite.pane, Some(PaneId(3)));
        assert_eq!(vite.worktree, Some(wt));
        assert_eq!(vite.process.as_deref(), Some("node"));
        assert_eq!(vite.command.as_deref(), Some("node --serve"));
        let pg = &u.ports[1];
        assert_eq!((pg.pane, pg.worktree.clone()), (None, None));
        let api = &u.ports[2];
        assert_eq!(api.pane, None);
        assert_eq!(api.worktree, Some(main));
        assert_eq!(u.ports[3].pid, None);
    }

    #[test]
    fn worktree_prefix_matching() {
        assert!(path_within("/a/b", "/a/b"));
        assert!(path_within("/a/b/c", "/a/b/"));
        assert!(!path_within("/a/bc", "/a/b"));
    }

    #[test]
    fn long_commands_are_truncated() {
        let long = "x".repeat(MAX_PORT_COMMAND_CHARS + 50);
        let t = truncate_command(&long);
        assert_eq!(t.chars().count(), MAX_PORT_COMMAND_CHARS);
        assert!(t.ends_with('…'));
    }

    #[tokio::test]
    async fn dials_ipv4_and_ipv6_loopback() {
        // Forwarding itself (and refusal) is exercised end to end in
        // tests/ports.rs; here only that both loopback families are tried.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(connect_loopback(port).await.is_ok());
        if let Ok(l6) = tokio::net::TcpListener::bind("[::1]:0").await {
            let port6 = l6.local_addr().unwrap().port();
            assert!(connect_loopback(port6).await.is_ok());
        }
    }
}
