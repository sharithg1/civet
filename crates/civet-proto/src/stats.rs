//! Host resource stats (CPU, memory, disks, network, GPUs, and what each
//! pane's processes use), for the desktop's btop-style stats view.
//!
//! The client polls [`crate::Request::HostStats`] on the control stream, so
//! it works on every link (QUIC and the SSH-tunnel fallback). The daemon
//! samples every [`HostStats::interval_ms`] while anyone polls and keeps a
//! short history, so graphs are full as soon as they open. Clients send it
//! only to daemons of at least [`crate::HOST_STATS_MIN_DAEMON_VERSION`].
//!
//! Integers only (the messages derive `Eq`): CPU and GPU busy as permille,
//! memory and disks in bytes, rates in bytes per second.

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, WorktreeId};

/// Samples the daemon keeps (10 minutes at the default 2 s interval).
pub const MAX_STATS_SAMPLES: usize = 300;

/// Most [`ProcessUsage`] entries in one [`HostStats`].
pub const MAX_STATS_PROCESSES: usize = 400;

/// Longest [`ProcessUsage::command`], in chars.
pub const MAX_STATS_COMMAND_CHARS: usize = 200;

/// One host-wide sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsSample {
    /// Increases by one per sample (per daemon run).
    pub seq: u64,
    /// Daemon wall clock, ms since epoch.
    pub at_ms: u64,
    /// All CPUs together, permille busy (0..=1000).
    pub cpu: u16,
    /// Memory in use (total minus available), bytes.
    pub mem_used: u64,
    /// Swap in use, bytes.
    pub swap_used: u64,
    /// Received on all non-loopback interfaces, bytes per second.
    pub net_rx: u64,
    /// Sent on all non-loopback interfaces, bytes per second.
    pub net_tx: u64,
    /// Read from all disks, bytes per second.
    pub disk_read: u64,
    /// Written to all disks, bytes per second.
    pub disk_write: u64,
    /// Each GPU's utilization, permille, in [`HostStats::gpus`] order.
    pub gpus: Vec<u16>,
}

/// A mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpace {
    /// Mount point (`/`, `/home`...).
    pub mount: String,
    /// Size, bytes.
    pub total: u64,
    /// Free for the daemon's user, bytes.
    pub available: u64,
}

/// A GPU (NVIDIA only, from `nvidia-smi`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    /// Model name.
    pub name: String,
    /// Memory in use, bytes.
    pub mem_used: u64,
    /// Memory size, bytes.
    pub mem_total: u64,
}

/// One process and what it uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessUsage {
    /// Process id.
    pub pid: u32,
    /// Process name (`node`, `cargo`...).
    pub name: String,
    /// Command line, truncated to [`MAX_STATS_COMMAND_CHARS`].
    pub command: String,
    /// CPU, permille of one core (above 1000 when it uses several).
    pub cpu: u32,
    /// Resident memory, bytes.
    pub mem: u64,
    /// Pane whose process tree it is in.
    pub pane: Option<PaneId>,
    /// That pane's worktree.
    pub worktree: Option<WorktreeId>,
}

/// Reply to [`crate::Request::HostStats`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStats {
    /// Time between samples, ms.
    pub interval_ms: u32,
    /// Samples newer than the request's `since` (all of them without it),
    /// oldest first, at most [`MAX_STATS_SAMPLES`]. Empty while the first
    /// one is still being taken.
    pub samples: Vec<StatsSample>,
    /// Each CPU's busy permille, as of the newest sample.
    pub cores: Vec<u16>,
    /// Physical memory, bytes.
    pub mem_total: u64,
    /// Swap size, bytes.
    pub swap_total: u64,
    /// 1, 5 and 15 minute load averages, in hundredths.
    pub load: [u32; 3],
    /// Seconds since boot.
    pub uptime_secs: u64,
    /// Local filesystems, largest first.
    pub disks: Vec<DiskSpace>,
    /// GPUs (empty without `nvidia-smi`).
    pub gpus: Vec<GpuInfo>,
    /// Only when the request asked for `processes`: every process in a
    /// pane's tree, then the busiest others, CPU first, at most
    /// [`MAX_STATS_PROCESSES`]. Empty until a second process sample (CPU
    /// use is measured between two).
    pub processes: Vec<ProcessUsage>,
}
