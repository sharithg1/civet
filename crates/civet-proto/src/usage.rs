//! Agent token usage and estimated spend, read on the host from the agents'
//! own session logs (Claude Code's `~/.claude/projects`, Codex's
//! `~/.codex/sessions`), for the desktop's usage view and the per-worktree
//! figures in the sidebar.
//!
//! The client polls [`crate::Request::AgentUsage`] on the control stream.
//! The daemon only reads those logs (it never writes to them) and keeps
//! what it parsed, so a poll only reads what was appended since the last.
//! Clients send it only to daemons of at least
//! [`crate::AGENT_USAGE_MIN_DAEMON_VERSION`].
//!
//! Costs are **estimates** at the list prices in [`AgentUsage::prices`]
//! (the daemon's table, as of [`AgentUsage::prices_as_of`]), not a bill:
//! subscriptions, discounts and batch pricing are not known. Integers only
//! (the messages derive `Eq`): money in micro-dollars, times in ms since
//! epoch.

use serde::{Deserialize, Serialize};

use crate::ids::{RepoId, WorktreeId};

/// Most [`AgentSessionUsage`] entries in one [`AgentUsage`] (the newest).
pub const MAX_USAGE_SESSIONS: usize = 2000;

/// Which agent CLI wrote a session log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsageAgent {
    /// Claude Code (`~/.claude/projects/<dir>/<session>.jsonl`, subagent
    /// logs included).
    Claude,
    /// Codex (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
    Codex,
}

/// Token counts. `input` excludes cache reads and writes, so the four add up
/// to everything the model read and wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    /// Uncached input.
    pub input: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// Written to the prompt cache (Claude only; Codex reports none).
    pub cache_write: u64,
    /// Read from the prompt cache.
    pub cache_read: u64,
}

impl TokenCounts {
    /// Everything together.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }

    /// Add `o` in place.
    pub fn add(&mut self, o: &TokenCounts) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
    }
}

/// One hour of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageHour {
    /// Start of the hour (UTC), ms since epoch. Clients roll hours up into
    /// their own local days.
    pub hour_ms: u64,
    /// Tokens used in it.
    pub tokens: TokenCounts,
    /// Estimated cost, micro-dollars.
    pub cost_micros: u64,
}

/// One agent session (a Claude Code conversation with its subagents, or a
/// Codex rollout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionUsage {
    /// Which agent.
    pub agent: UsageAgent,
    /// The agent's session id.
    pub session_id: String,
    /// Directory the session ran in.
    pub cwd: String,
    /// The worktree containing `cwd` (the deepest one), if any project of
    /// the daemon's has it.
    pub worktree: Option<WorktreeId>,
    /// That worktree's project.
    pub repo: Option<RepoId>,
    /// Models used, most tokens first.
    pub models: Vec<String>,
    /// First and last usage, ms since epoch.
    pub first_ms: u64,
    /// See `first_ms`.
    pub last_ms: u64,
    /// Everything in the requested window.
    pub tokens: TokenCounts,
    /// Estimated cost of `tokens`, micro-dollars.
    pub cost_micros: u64,
    /// Tokens of models missing from the price table (not in `cost_micros`).
    pub unpriced_tokens: u64,
    /// Hour by hour, oldest first.
    pub hours: Vec<UsageHour>,
}

/// A row of the daemon's price table, US dollars per million tokens in
/// micro-dollars (so `4_000_000` is $4 / MTok).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    /// Model id (`claude-opus-5-5`, `gpt-5-codex`); dated ids
    /// (`…-20250929`) use their base model's row.
    pub model: String,
    /// Uncached input.
    pub input: u64,
    /// Output.
    pub output: u64,
    /// Cache writes with the 5-minute lifetime.
    pub cache_write_5m: u64,
    /// Cache writes with the 1-hour lifetime.
    pub cache_write_1h: u64,
    /// Cache reads.
    pub cache_read: u64,
}

/// Reply to [`crate::Request::AgentUsage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    /// The request's `since_ms`: nothing older is counted.
    pub since_ms: u64,
    /// Daemon wall clock when the logs were read, ms since epoch.
    pub scanned_at_ms: u64,
    /// Sessions with usage since `since_ms`, newest activity first, at most
    /// [`MAX_USAGE_SESSIONS`].
    pub sessions: Vec<AgentSessionUsage>,
    /// The price table the costs were estimated with.
    pub prices: Vec<ModelPrice>,
    /// When the price table was last checked (`YYYY-MM-DD`).
    pub prices_as_of: String,
}
