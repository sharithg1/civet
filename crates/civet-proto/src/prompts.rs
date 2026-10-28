//! Prompts typed into agent panes by the daemon on the user's behalf:
//! sent now to one or several panes ([`crate::Request::SendPrompt`],
//! broadcast), or queued on a pane and sent the next time its agent stops
//! ([`crate::Request::QueuePrompt`]).
//!
//! The daemon owns the queue, so a queued prompt fires while the desktop is
//! closed or asleep. It types the text the way a paste would (bracketed
//! paste when the program asked for it), then presses Enter a moment later,
//! so TUIs like Claude Code and Codex take it as one message.

use serde::{Deserialize, Serialize};

use crate::agent::AgentStatus;
use crate::ids::PaneId;

/// When a [`QueuedPrompt`] is sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PromptTrigger {
    /// The next time the agent is done (its turn ended).
    #[default]
    Done,
    /// The next time the agent is done or needs input (a question or a
    /// permission prompt: the text answers it).
    DoneOrNeedsInput,
}

impl PromptTrigger {
    /// True if an agent entering `status` fires a prompt with this trigger.
    pub fn fires_on(self, status: AgentStatus) -> bool {
        match self {
            PromptTrigger::Done => status == AgentStatus::Done,
            PromptTrigger::DoneOrNeedsInput => status.wants_attention(),
        }
    }
}

/// A prompt waiting on a pane ([`crate::Request::QueuePrompt`]). A pane's
/// prompts are sent one at a time, oldest first: each time the agent enters
/// a status that the first prompt's trigger fires on (and is still in it a
/// moment later), that prompt is typed and submitted and the next one waits
/// for the following turn. A pane's queue is dropped when the pane exits or
/// is closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedPrompt {
    /// Daemon-wide id (never reused by one daemon instance).
    pub id: u64,
    /// Pane it is sent to.
    pub pane: PaneId,
    /// Text to type (at most [`MAX_PROMPT_BYTES`]).
    pub text: String,
    /// When it is sent.
    pub trigger: PromptTrigger,
    /// When it was queued, ms since epoch.
    pub created_at_ms: u64,
}

/// Longest prompt text the daemon accepts, in bytes.
pub const MAX_PROMPT_BYTES: usize = 64 * 1024;

/// Most prompts queued on one pane.
pub const MAX_QUEUED_PROMPTS: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers() {
        assert!(PromptTrigger::Done.fires_on(AgentStatus::Done));
        assert!(!PromptTrigger::Done.fires_on(AgentStatus::NeedsInput));
        assert!(!PromptTrigger::Done.fires_on(AgentStatus::Idle));
        assert!(PromptTrigger::DoneOrNeedsInput.fires_on(AgentStatus::Done));
        assert!(PromptTrigger::DoneOrNeedsInput.fires_on(AgentStatus::NeedsInput));
        assert!(!PromptTrigger::DoneOrNeedsInput.fires_on(AgentStatus::Running));
    }

    #[test]
    fn json_names() {
        assert_eq!(
            serde_json::to_string(&PromptTrigger::DoneOrNeedsInput).unwrap(),
            "\"doneOrNeedsInput\""
        );
        let p = QueuedPrompt {
            id: 3,
            pane: PaneId(7),
            text: "run the tests".into(),
            trigger: PromptTrigger::Done,
            created_at_ms: 1,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["createdAtMs"], 1);
        assert_eq!(v["trigger"], "done");
    }
}
