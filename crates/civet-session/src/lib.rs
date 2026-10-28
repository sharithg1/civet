//! Session manager for Civet.
//!
//! Owns everything that lives on the remote host and survives client
//! disconnects:
//!
//! * **Repos and worktrees**, managed by running the `git` CLI
//!   ([`SessionManager::add_repo`], [`SessionManager::create_worktree`], ...).
//! * **Panes**: one PTY (portable-pty) per pane whose output is fed into a
//!   server-side [`civet_term::PaneTerminal`]. Each pane has a reader thread
//!   (PTY → terminal), a writer thread (input and terminal replies → PTY) and
//!   a waiter thread (process exit).
//! * **Input sequencing**: [`SessionManager::write_input`] applies each
//!   per-(client, pane) sequence number at most once, so clients may resend
//!   unacknowledged input after a resumed reconnect.
//! * **Screen subscriptions** with mosh-style flow control ([`Subscription`]):
//!   newest-wins diffs, coalescing, a bounded in-flight window and echo acks.
//! * **Agent status detection** (process exit, hook file, OSC 9/777/133, BEL
//!   and idle heuristics), pushed as [`civet_proto::Event::AgentStatus`].
//! * **Agent activity timeline** per worktree: each turn's times, status and
//!   the files it changed (see `activity.rs`).
//!
//! There is no networking here; `civetd` wires this to the transport.
//! See `docs/INTERFACES.md` (section 4) for the contract and `HOOKS.md` in
//! this crate for wiring agent hooks to `$CIVET_STATUS_FILE`.

mod activity;
mod changes;
mod error;
mod fileops;
mod files;
mod git;
mod git_actions;
mod github;
mod hooks;
mod manager;
mod pane;
mod status;
mod subscription;
mod sys;
mod watch;

use std::path::PathBuf;
use std::time::Duration;

pub use error::SessionError;
pub use manager::SessionManager;
pub use subscription::{PaneOutput, Subscription};

/// Program basenames treated as agent CLIs for status heuristics even when
/// the spawn request carries no `agent` hint.
pub const KNOWN_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "amp",
    "cursor-agent",
    "goose",
];

/// Environment variable set in panes spawned inside a worktree.
pub const ENV_WORKTREE: &str = "CIVET_WORKTREE";

/// Configuration for a [`SessionManager`].
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// `~/.civet` (or `$CIVET_STATE_DIR`). Holds `repos.json`, `status/`,
    /// `hooks/`, `trash/` and `activity/`.
    pub state_dir: PathBuf,
    /// Shell for panes without a command and for `-lc` wrapping.
    /// Default (None): `$SHELL`, else `/bin/bash`, else `/bin/sh`.
    pub shell: Option<String>,
    /// Per-pane scrollback store limit. Default 10_000.
    pub scrollback_limit: usize,
    /// Quiet period before an agent pane goes Running -> Idle. Default 8 s.
    pub idle_after: Duration,
    /// Path of the git binary. Default `"git"`.
    pub git: PathBuf,
    /// How often worktree metadata is checked for changes made outside
    /// Civet (then [`civet_proto::Event::WorktreesChanged`]). Default 2 s.
    pub worktree_poll: Duration,
    /// Program behind the `civet` PATH shim in panes (`<state>/hooks/bin/
    /// civet`, e.g. `civet handoff`): the daemon's own binary. None = no
    /// `civet` shim. Default None.
    pub cli_program: Option<PathBuf>,
}

impl SessionConfig {
    /// Defaults with an explicit state directory.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            shell: None,
            scrollback_limit: 10_000,
            idle_after: Duration::from_secs(8),
            git: PathBuf::from("git"),
            worktree_poll: Duration::from_secs(2),
            cli_program: None,
        }
    }

    /// The state directory from `$CIVET_STATE_DIR`, else `~/.civet`, else
    /// `./.civet` if the home directory is unknown.
    pub fn default_state_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os(civet_proto::ENV_STATE_DIR).filter(|d| !d.is_empty()) {
            return PathBuf::from(dir);
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(civet_proto::STATE_DIR_NAME)
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self::new(Self::default_state_dir())
    }
}

/// Result of [`SessionManager::write_input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// The bytes were queued for the PTY and the seq recorded.
    Applied,
    /// `seq` was not greater than the last applied one; nothing was written.
    Duplicate,
}
