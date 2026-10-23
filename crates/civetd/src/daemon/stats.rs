//! Host stats for `Request::HostStats` (see `civet_proto::stats`).
//!
//! The sampler runs only while clients poll: the first request starts it,
//! and it stops (dropping its history) after [`IDLE_STOP`] without one.
//! Every [`INTERVAL`] it records CPU, memory, swap, network and disk I/O
//! (sysinfo, on Linux and macOS) plus GPUs (`nvidia-smi`, if present) into a
//! ring of [`MAX_STATS_SAMPLES`]. Processes are only sampled while a client
//! asked for them in the last [`PROCESS_IDLE`] (the stats view is open):
//! each one is attributed to the pane whose process tree it is in.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use civet_proto::{
    DiskSpace, GpuInfo, HostStats, MAX_STATS_COMMAND_CHARS, MAX_STATS_PROCESSES, MAX_STATS_SAMPLES,
    PaneId, ProcessUsage, StatsSample, WorktreeId, now_ms,
};
use civet_session::SessionManager;
use sysinfo::{
    DiskRefreshKind, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System,
    UpdateKind,
};
use tokio::sync::{Notify, watch};

/// Time between samples.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// The sampler stops this long after the last request.
const IDLE_STOP: Duration = Duration::from_secs(60);

/// Processes are sampled while one was asked for within this long.
const PROCESS_IDLE: Duration = Duration::from_secs(10);

/// How long a request waits for the very first sample (listing disks the
/// first time takes about 2 s on macOS).
const FIRST_SAMPLE_WAIT: Duration = Duration::from_secs(4);

/// How long `nvidia-smi` may take before it is killed (and given up on).
const GPU_QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Parent links followed when attributing a process to a pane.
const MAX_ANCESTRY: usize = 64;

/// Processes outside every pane listed (the busiest ones).
const MAX_OTHER_PROCESSES: usize = 40;

/// Filesystems that are not disks of their own.
const PSEUDO_FS: &[&str] = &[
    "autofs", "devfs", "devtmpfs", "nsfs", "overlay", "proc", "ramfs", "squashfs", "sysfs", "tmpfs",
];

/// Mounts that only repeat (part of) another one: macOS system volumes,
/// snaps, container layers, EFI.
const SKIP_MOUNT_PREFIXES: &[&str] = &[
    "/System/Volumes/",
    "/private/var/vm",
    "/snap/",
    "/var/lib/docker/",
    "/boot/efi",
];

/// Shared stats state: the history and whether the sampler runs.
pub(crate) struct StatsSampler {
    sessions: SessionManager,
    state: Mutex<State>,
    /// Newest sample's seq; wakes requests waiting for the first one.
    tick: watch::Sender<u64>,
    /// Cuts the sampler's sleep short when processes are first asked for.
    wake: Notify,
}

impl std::fmt::Debug for StatsSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsSampler").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    running: bool,
    last_request: Option<Instant>,
    last_process_request: Option<Instant>,
    samples: VecDeque<StatsSample>,
    /// Everything but the samples, as of the newest one.
    latest: Option<HostStats>,
    next_seq: u64,
}

impl StatsSampler {
    pub(crate) fn new(sessions: SessionManager) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            state: Mutex::new(State::default()),
            tick: watch::channel(0).0,
            wake: Notify::new(),
        })
    }

    /// Answer one `Request::HostStats`.
    pub(crate) async fn request(
        self: &Arc<Self>,
        since: Option<u64>,
        processes: bool,
    ) -> HostStats {
        let mut rx = self.tick.subscribe();
        let empty = {
            let mut st = self.lock();
            let now = Instant::now();
            st.last_request = Some(now);
            let wanted_processes = st
                .last_process_request
                .is_some_and(|t| t.elapsed() < PROCESS_IDLE);
            if processes {
                st.last_process_request = Some(now);
            }
            if !st.running {
                st.running = true;
                tokio::spawn(sampler(Arc::downgrade(self)));
            } else if processes && !wanted_processes {
                self.wake.notify_one();
            }
            st.samples.is_empty()
        };
        if empty {
            let _ = tokio::time::timeout(FIRST_SAMPLE_WAIT, rx.changed()).await;
        }
        let st = self.lock();
        respond(&st, since, processes)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The reply to a request from the state as it is.
fn respond(st: &State, since: Option<u64>, processes: bool) -> HostStats {
    let mut out = st.latest.clone().unwrap_or_else(|| HostStats {
        interval_ms: INTERVAL.as_millis() as u32,
        samples: Vec::new(),
        cores: Vec::new(),
        mem_total: 0,
        swap_total: 0,
        load: [0; 3],
        uptime_secs: 0,
        disks: Vec::new(),
        gpus: Vec::new(),
        processes: Vec::new(),
    });
    out.samples = samples_since(&st.samples, since);
    if !processes {
        out.processes.clear();
    }
    out
}

/// Samples newer than `since`; all of them if `since` is newer than the
/// newest one (the client saw another daemon run).
fn samples_since(samples: &VecDeque<StatsSample>, since: Option<u64>) -> Vec<StatsSample> {
    let newest = samples.back().map_or(0, |s| s.seq);
    match since {
        Some(s) if s <= newest => samples.iter().filter(|x| x.seq > s).cloned().collect(),
        _ => samples.iter().cloned().collect(),
    }
}

/// Sample every [`INTERVAL`] while anyone polls, then stop.
async fn sampler(weak: Weak<StatsSampler>) {
    tracing::debug!("stats sampler started");
    let mut probe: Option<Probe> = None;
    loop {
        let Some(s) = weak.upgrade() else { return };
        let processes = {
            let mut st = s.lock();
            if st.last_request.is_none_or(|t| t.elapsed() > IDLE_STOP) {
                st.running = false;
                st.samples.clear();
                st.latest = None;
                tracing::debug!("stats sampler stopped (no requests)");
                return;
            }
            st.last_process_request
                .is_some_and(|t| t.elapsed() < PROCESS_IDLE)
        };
        let panes = s.sessions.pane_processes();
        let (p, reading) = match tokio::task::spawn_blocking(move || {
            let mut probe = probe.unwrap_or_else(Probe::new);
            let r = probe.sample(processes, &panes);
            (probe, r)
        })
        .await
        {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "stats sample panicked");
                s.lock().running = false;
                return;
            }
        };
        probe = Some(p);
        let seq = {
            let mut st = s.lock();
            let seq = st.next_seq;
            st.next_seq += 1;
            let (sample, latest) = reading.into_parts(seq);
            st.samples.push_back(sample);
            while st.samples.len() > MAX_STATS_SAMPLES {
                st.samples.pop_front();
            }
            st.latest = Some(latest);
            seq
        };
        s.tick.send_replace(seq);
        let wake = async { s.wake.notified().await };
        tokio::select! {
            () = tokio::time::sleep(INTERVAL) => {}
            () = wake => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Probing
// ---------------------------------------------------------------------------

/// What one sample measured.
struct Reading {
    at_ms: u64,
    cpu: u16,
    cores: Vec<u16>,
    mem_total: u64,
    mem_used: u64,
    swap_total: u64,
    swap_used: u64,
    net_rx: u64,
    net_tx: u64,
    disk_read: u64,
    disk_write: u64,
    load: [u32; 3],
    uptime_secs: u64,
    disks: Vec<DiskSpace>,
    gpus: Vec<(GpuInfo, u16)>,
    processes: Vec<ProcessUsage>,
}

impl Reading {
    fn into_parts(self, seq: u64) -> (StatsSample, HostStats) {
        let (gpus, gpu_busy): (Vec<GpuInfo>, Vec<u16>) = self.gpus.into_iter().unzip();
        (
            StatsSample {
                seq,
                at_ms: self.at_ms,
                cpu: self.cpu,
                mem_used: self.mem_used,
                swap_used: self.swap_used,
                net_rx: self.net_rx,
                net_tx: self.net_tx,
                disk_read: self.disk_read,
                disk_write: self.disk_write,
                gpus: gpu_busy,
            },
            HostStats {
                interval_ms: INTERVAL.as_millis() as u32,
                samples: Vec::new(),
                cores: self.cores,
                mem_total: self.mem_total,
                swap_total: self.swap_total,
                load: self.load,
                uptime_secs: self.uptime_secs,
                disks: self.disks,
                gpus,
                processes: self.processes,
            },
        )
    }
}

/// The sysinfo handles, kept between samples (rates are deltas).
struct Probe {
    sys: System,
    networks: Networks,
    disks: Disks,
    last: Instant,
    /// Processes were refreshed by the previous sample too, so their CPU
    /// use is measured over one interval.
    processes_warm: bool,
    /// False once `nvidia-smi` is missing or failed.
    gpus: bool,
}

impl Probe {
    fn new() -> Self {
        // sysinfo keeps every /proc/<pid>/stat open by default on Linux;
        // the daemon would hold thousands of descriptors.
        sysinfo::set_open_files_limit(0);
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        let networks = Networks::new_with_refreshed_list();
        let disks = Disks::new_with_refreshed_list_specifics(disk_refresh());
        // Network and disk rates are deltas since these baselines.
        let last = Instant::now();
        // CPU use is a delta: give the first sample something to compare.
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        Self {
            sys,
            networks,
            disks,
            last,
            processes_warm: false,
            gpus: true,
        }
    }

    fn sample(&mut self, processes: bool, panes: &[(PaneId, u32, Option<WorktreeId>)]) -> Reading {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh_specifics(true, disk_refresh());
        let secs = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        let rate = |bytes: u64| (bytes as f64 / secs) as u64;

        let (mut rx, mut tx) = (0, 0);
        for (name, data) in self.networks.list() {
            if !is_loopback(name) {
                rx += data.received();
                tx += data.transmitted();
            }
        }
        let (mut read, mut written) = (0, 0);
        let mut seen = HashSet::new();
        for d in self.disks.list() {
            // One device mounted twice (bind mounts) counts once.
            if seen.insert(d.name().to_os_string()) {
                let u = d.usage();
                read += u.read_bytes;
                written += u.written_bytes;
            }
        }

        let process_list = if processes {
            let refresh = ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .without_tasks();
            if !self.processes_warm {
                self.sys
                    .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
                std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            }
            self.sys
                .refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
            self.processes_warm = true;
            let procs: HashMap<u32, Proc> = self
                .sys
                .processes()
                .values()
                .map(|p| {
                    let pid = p.pid().as_u32();
                    let command = p
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let name = p.name().to_string_lossy().into_owned();
                    (
                        pid,
                        Proc {
                            name: name.clone(),
                            command: if command.is_empty() { name } else { command },
                            ppid: p.parent().map(Pid::as_u32),
                            cpu: (f64::from(p.cpu_usage()) * 10.0).round() as u32,
                            mem: p.memory(),
                        },
                    )
                })
                .collect();
            attribute(&procs, panes)
        } else {
            // The next view measures from two fresh refreshes again.
            self.processes_warm = false;
            Vec::new()
        };

        let gpus = if self.gpus {
            let g = query_gpus();
            self.gpus = g.is_some();
            g.unwrap_or_default()
        } else {
            Vec::new()
        };

        let load = System::load_average();
        let hundredths = |l: f64| (l * 100.0).round() as u32;
        Reading {
            at_ms: now_ms(),
            cpu: permille(self.sys.global_cpu_usage()),
            cores: self
                .sys
                .cpus()
                .iter()
                .map(|c| permille(c.cpu_usage()))
                .collect(),
            mem_total: self.sys.total_memory(),
            mem_used: self.sys.used_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            net_rx: rate(rx),
            net_tx: rate(tx),
            disk_read: rate(read),
            disk_write: rate(written),
            load: [
                hundredths(load.one),
                hundredths(load.five),
                hundredths(load.fifteen),
            ],
            uptime_secs: System::uptime(),
            disks: disk_space(&self.disks),
            gpus,
            processes: process_list,
        }
    }
}

fn disk_refresh() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage().with_io_usage()
}

fn is_loopback(interface: &str) -> bool {
    interface == "lo" || interface.starts_with("lo0")
}

/// A sysinfo percentage (0..=100) as permille.
fn permille(percent: f32) -> u16 {
    (f64::from(percent) * 10.0).round().clamp(0.0, 1000.0) as u16
}

/// Real, fixed filesystems (no USB sticks or mounted disk images), one per
/// device, largest first.
fn disk_space(disks: &Disks) -> Vec<DiskSpace> {
    let mut by_device: HashMap<std::ffi::OsString, DiskSpace> = HashMap::new();
    for d in disks.list() {
        let fs = d.file_system().to_string_lossy();
        let mount = d.mount_point().to_string_lossy().into_owned();
        if d.is_removable() || !keep_disk(&fs, &mount, d.total_space()) {
            continue;
        }
        let entry = DiskSpace {
            mount,
            total: d.total_space(),
            available: d.available_space(),
        };
        // A device mounted several times (bind mounts) shows its shortest
        // mount point.
        by_device
            .entry(d.name().to_os_string())
            .and_modify(|e| {
                if entry.mount.len() < e.mount.len() {
                    *e = entry.clone();
                }
            })
            .or_insert(entry);
    }
    let mut out: Vec<DiskSpace> = by_device.into_values().collect();
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.mount.cmp(&b.mount)));
    out
}

fn keep_disk(fs: &str, mount: &str, total: u64) -> bool {
    total > 0
        && !PSEUDO_FS.contains(&fs)
        && !fs.starts_with("fuse.snap")
        && !SKIP_MOUNT_PREFIXES
            .iter()
            .any(|p| mount.starts_with(p) || mount == p.trim_end_matches('/'))
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Proc {
    name: String,
    command: String,
    ppid: Option<u32>,
    /// Permille of one core.
    cpu: u32,
    mem: u64,
}

/// Every process in a pane's tree, then the busiest others (kernel
/// threads, which have no memory, left out), CPU first.
fn attribute(
    procs: &HashMap<u32, Proc>,
    panes: &[(PaneId, u32, Option<WorktreeId>)],
) -> Vec<ProcessUsage> {
    let pane_of_pid: HashMap<u32, (PaneId, Option<WorktreeId>)> = panes
        .iter()
        .map(|(id, pid, wt)| (*pid, (*id, wt.clone())))
        .collect();
    let mut memo: HashMap<u32, Option<(PaneId, Option<WorktreeId>)>> = HashMap::new();
    let mut in_panes = Vec::new();
    let mut others = Vec::new();
    for (&pid, p) in procs {
        let usage = |pane: Option<PaneId>, worktree: Option<WorktreeId>| ProcessUsage {
            pid,
            name: p.name.clone(),
            command: truncate(&p.command),
            cpu: p.cpu,
            mem: p.mem,
            pane,
            worktree,
        };
        match pane_of(pid, procs, &pane_of_pid, &mut memo) {
            Some((pane, wt)) => in_panes.push(usage(Some(pane), wt)),
            None if p.mem > 0 => others.push(usage(None, None)),
            None => {}
        }
    }
    let busiest = |a: &ProcessUsage, b: &ProcessUsage| {
        b.cpu
            .cmp(&a.cpu)
            .then(b.mem.cmp(&a.mem))
            .then(a.pid.cmp(&b.pid))
    };
    in_panes.sort_by(busiest);
    others.sort_by(busiest);
    others.truncate(MAX_OTHER_PROCESSES);
    in_panes.extend(others);
    in_panes.truncate(MAX_STATS_PROCESSES);
    in_panes
}

fn pane_of(
    pid: u32,
    procs: &HashMap<u32, Proc>,
    panes: &HashMap<u32, (PaneId, Option<WorktreeId>)>,
    memo: &mut HashMap<u32, Option<(PaneId, Option<WorktreeId>)>>,
) -> Option<(PaneId, Option<WorktreeId>)> {
    let mut chain = Vec::new();
    let mut cur = pid;
    let mut found = None;
    for _ in 0..MAX_ANCESTRY {
        if let Some(known) = memo.get(&cur) {
            found = known.clone();
            break;
        }
        if let Some(p) = panes.get(&cur) {
            found = Some(p.clone());
            break;
        }
        chain.push(cur);
        match procs.get(&cur).and_then(|p| p.ppid) {
            Some(p) if p > 1 && p != cur => cur = p,
            _ => break,
        }
    }
    for c in chain {
        memo.insert(c, found.clone());
    }
    found
}

fn truncate(c: &str) -> String {
    let c = c.trim();
    if c.chars().count() <= MAX_STATS_COMMAND_CHARS {
        return c.to_string();
    }
    let mut s: String = c.chars().take(MAX_STATS_COMMAND_CHARS - 1).collect();
    s.push('…');
    s
}

// ---------------------------------------------------------------------------
// GPUs
// ---------------------------------------------------------------------------

/// NVIDIA GPUs and their utilization; None if `nvidia-smi` is missing,
/// fails or hangs (not tried again this sampler run).
fn query_gpus() -> Option<Vec<(GpuInfo, u16)>> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GPU_QUERY_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!("nvidia-smi timed out; GPU stats off");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(parse_nvidia_smi(&out))
}

/// `name, util%, used MiB, total MiB` per line.
fn parse_nvidia_smi(text: &str) -> Vec<(GpuInfo, u16)> {
    const MIB: u64 = 1 << 20;
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, util, used, total] = f[..] else {
                return None;
            };
            let num = |s: &str| s.parse::<u64>().ok();
            let util = num(util).unwrap_or(0).min(100) as u16 * 10;
            Some((
                GpuInfo {
                    name: name.to_string(),
                    mem_used: num(used)? * MIB,
                    mem_total: num(total)? * MIB,
                },
                util,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seq: u64) -> StatsSample {
        StatsSample {
            seq,
            at_ms: seq * 2000,
            cpu: 0,
            mem_used: 0,
            swap_used: 0,
            net_rx: 0,
            net_tx: 0,
            disk_read: 0,
            disk_write: 0,
            gpus: vec![],
        }
    }

    fn seqs(v: &[StatsSample]) -> Vec<u64> {
        v.iter().map(|s| s.seq).collect()
    }

    #[test]
    fn samples_since_slices_history() {
        let h: VecDeque<StatsSample> = (5..10).map(sample).collect();
        assert_eq!(seqs(&samples_since(&h, None)), vec![5, 6, 7, 8, 9]);
        assert_eq!(seqs(&samples_since(&h, Some(7))), vec![8, 9]);
        assert_eq!(seqs(&samples_since(&h, Some(9))), Vec::<u64>::new());
        // Older than the history: whatever is left.
        assert_eq!(seqs(&samples_since(&h, Some(1))), vec![5, 6, 7, 8, 9]);
        // Newer than anything here: another daemon run; start over.
        assert_eq!(seqs(&samples_since(&h, Some(500))), vec![5, 6, 7, 8, 9]);
    }

    fn proc(name: &str, ppid: u32, cpu: u32, mem: u64) -> Proc {
        Proc {
            name: name.into(),
            command: name.into(),
            ppid: Some(ppid),
            cpu,
            mem,
        }
    }

    #[test]
    fn attributes_process_trees_to_panes() {
        let wt = WorktreeId::from("/w/feat");
        let procs: HashMap<u32, Proc> = [
            (100, proc("zsh", 1, 0, 10)),
            (101, proc("claude", 100, 50, 300)),
            (102, proc("cargo", 101, 3400, 900)),
            (103, proc("rustc", 102, 900, 500)),
            (200, proc("dockerd", 1, 800, 400)),
            (201, proc("kworker", 2, 10, 0)),
            (300, proc("sshd", 1, 0, 20)),
        ]
        .into_iter()
        .collect();
        let panes = [(PaneId(7), 100, Some(wt.clone()))];
        let out = attribute(&procs, &panes);
        let summary: Vec<(u32, Option<PaneId>)> = out.iter().map(|p| (p.pid, p.pane)).collect();
        assert_eq!(
            summary,
            vec![
                (102, Some(PaneId(7))),
                (103, Some(PaneId(7))),
                (101, Some(PaneId(7))),
                (100, Some(PaneId(7))),
                (200, None),
                (300, None),
            ]
        );
        assert_eq!(out[0].worktree.as_ref(), Some(&wt));
        assert_eq!(out[4].worktree, None);
    }

    #[test]
    fn parses_nvidia_smi() {
        let g = parse_nvidia_smi(
            "NVIDIA A10G, 88, 20480, 23028\nNVIDIA A10G, 0, 3, 23028\n\ngarbage line\n",
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0.name, "NVIDIA A10G");
        assert_eq!(g[0].1, 880);
        assert_eq!(g[0].0.mem_used, 20480 << 20);
        assert_eq!(g[1].1, 0);
    }

    #[test]
    fn keeps_real_disks_only() {
        assert!(keep_disk("ext4", "/", 1 << 40));
        assert!(keep_disk("apfs", "/", 1 << 40));
        assert!(keep_disk("xfs", "/data", 1 << 40));
        assert!(!keep_disk("overlay", "/", 1 << 40));
        assert!(!keep_disk("squashfs", "/snap/core/1", 1 << 30));
        assert!(!keep_disk("apfs", "/System/Volumes/Data", 1 << 40));
        assert!(!keep_disk("vfat", "/boot/efi", 1 << 29));
        assert!(!keep_disk("ext4", "/x", 0));
    }

    #[test]
    fn permille_clamps() {
        assert_eq!(permille(62.54), 625);
        assert_eq!(permille(-1.0), 0);
        assert_eq!(permille(250.0), 1000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn measures_this_host() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = SessionManager::new(civet_session::SessionConfig::new(dir.path())).unwrap();
        let s = StatsSampler::new(sessions);
        let first = s.request(None, true).await;
        assert_eq!(first.samples.len(), 1, "first request waits for a sample");
        assert!(first.mem_total > 0);
        assert!(first.samples[0].mem_used > 0);
        assert!(!first.cores.is_empty());
        let seq = first.samples[0].seq;
        let again = s.request(Some(seq), true).await;
        assert!(again.samples.iter().all(|x| x.seq > seq));
        assert!(!again.processes.is_empty());
        assert!(again.processes.iter().all(|p| p.pane.is_none()));
        assert!(s.request(None, false).await.processes.is_empty());
    }
}
