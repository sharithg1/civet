//! Agent status, as detected by the daemon for each pane.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What an agent CLI (Claude Code, Codex, any CLI) in a pane is doing.
///
/// JSON form is camelCase: `"running"`, `"needsInput"`, `"done"`, `"idle"`,
/// `"unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    /// No signal yet (fresh pane, or a plain shell nobody has classified).
    #[default]
    Unknown,
    /// Producing output / working.
    Running,
    /// Blocked waiting for the user (permission prompt, question, etc.).
    NeedsInput,
    /// Finished its task (or the process exited).
    Done,
    /// Alive but quiet with no pending prompt (e.g. at a shell prompt).
    Idle,
}

impl AgentStatus {
    /// True for states that should draw the user's attention (and, on a
    /// transition into them, may trigger a desktop notification).
    pub fn wants_attention(self) -> bool {
        matches!(self, AgentStatus::NeedsInput | AgentStatus::Done)
    }

    /// Stable lowercase label (same as the JSON form).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Unknown => "unknown",
            AgentStatus::Running => "running",
            AgentStatus::NeedsInput => "needsInput",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
        }
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which detector produced a status change. Useful for debugging and for the
/// UI to weigh confidence (hooks beat heuristics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatusSource {
    /// An escape sequence emitted by the agent (OSC 9 / OSC 777 notify,
    /// OSC 133 prompt marks, title changes, BEL...).
    EscapeCode,
    /// The agent's hook script wrote to the pane's hook/status file.
    Hook,
    /// Output-activity / idle-time heuristic.
    Heuristic,
    /// The pane's process exited.
    ProcessExit,
    /// Explicitly set by a client request.
    Client,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_names() {
        for s in [
            AgentStatus::Unknown,
            AgentStatus::Running,
            AgentStatus::NeedsInput,
            AgentStatus::Done,
            AgentStatus::Idle,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
            assert_eq!(serde_json::from_str::<AgentStatus>(&json).unwrap(), s);
            let bin = postcard::to_stdvec(&s).unwrap();
            assert_eq!(postcard::from_bytes::<AgentStatus>(&bin).unwrap(), s);
        }
    }

    #[test]
    fn attention() {
        assert!(AgentStatus::Done.wants_attention());
        assert!(AgentStatus::NeedsInput.wants_attention());
        assert!(!AgentStatus::Running.wants_attention());
    }
}
