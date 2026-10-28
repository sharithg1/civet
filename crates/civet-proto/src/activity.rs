//! Agent activity timeline: what the agents in a worktree did, turn by turn
//! ("what did my agent do while I was away?").
//!
//! The daemon records every agent pane's status transitions per worktree. A
//! [`AgentTurn`] is one stretch of `Running`, from the status turning
//! `Running` until it leaves it (`Done`, `NeedsInput`, `Idle`, the process
//! exiting or the pane closing). At both ends the daemon snapshots the
//! worktree's files as a git tree (a temporary index, so the user's index is
//! untouched) and lists what changed between the two trees, commits made
//! during the turn included. [`crate::Request::TurnDiff`] shows one file's
//! change within a turn.
//!
//! Timelines are kept on the host across daemon restarts, bounded to
//! [`MAX_ACTIVITY_TURNS`] turns per worktree and [`MAX_TURN_FILES`] files
//! per turn. Clients send the requests only to daemons of at least
//! [`crate::AGENT_ACTIVITY_MIN_DAEMON_VERSION`].

use serde::{Deserialize, Serialize};

use crate::agent::AgentStatus;
use crate::control::GitFileChange;
use crate::ids::PaneId;

/// Turns kept per worktree (oldest dropped first).
pub const MAX_ACTIVITY_TURNS: usize = 100;

/// Files listed per turn (`files_truncated` beyond).
pub const MAX_TURN_FILES: usize = 100;

/// One stretch of an agent working, as recorded by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurn {
    /// Increases by one per turn within a worktree (survives restarts).
    pub id: u64,
    /// The agent's pane. Pane ids restart with the daemon, so this only
    /// identifies a live pane for turns of the current daemon run.
    pub pane: PaneId,
    /// Who worked: the pane's agent hint, else its title at the start.
    pub agent: String,
    /// The prompt, when a hook reported it (Claude Code's prompt submit).
    pub prompt: Option<String>,
    /// When the status turned `Running`, ms since epoch.
    pub started_ms: u64,
    /// When it left `Running`; None while the turn runs.
    pub ended_ms: Option<u64>,
    /// `Running` while the turn runs; then the status it ended in. `Unknown`:
    /// the daemon stopped (or the pane closed) before the turn ended.
    pub status: AgentStatus,
    /// The status message at the end (a hook's or notification's text).
    pub message: Option<String>,
    /// Files that differ between the snapshots at the start and the end,
    /// sorted by path, with line counts. Empty while running.
    pub files: Vec<GitFileChange>,
    /// More than [`MAX_TURN_FILES`] files changed.
    pub files_truncated: bool,
    /// Git tree of the worktree at the start (None: not a git worktree, or
    /// the snapshot failed).
    pub from_tree: Option<String>,
    /// Git tree at the end.
    pub to_tree: Option<String>,
}

/// Reply to [`crate::Request::AgentActivity`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivity {
    /// Oldest first.
    pub turns: Vec<AgentTurn>,
}
