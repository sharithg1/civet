//! Agent status, as detected by the daemon for each pane.

use std::fmt;
use std::str::FromStr;

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

impl FromStr for AgentStatus {
    type Err = String;

    /// Lenient parser used for hook files and CLI flags. Case-insensitive;
    /// `-`, `_` and spaces are ignored. Accepts the JSON names plus common
    /// synonyms: `working`/`busy` (Running), `waiting`/`input`/`permission`
    /// (NeedsInput), `finished`/`complete`/`completed`/`stop`/`stopped`
    /// (Done).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm: String = s
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .flat_map(char::to_lowercase)
            .collect();
        Ok(match norm.as_str() {
            "unknown" => AgentStatus::Unknown,
            "running" | "working" | "busy" => AgentStatus::Running,
            "needsinput" | "waiting" | "input" | "permission" => AgentStatus::NeedsInput,
            "done" | "finished" | "complete" | "completed" | "stop" | "stopped" => {
                AgentStatus::Done
            }
            "idle" => AgentStatus::Idle,
            _ => return Err(format!("unknown agent status {s:?}")),
        })
    }
}

/// Parse the contents of a pane's `$CIVET_STATUS_FILE` (written by agent
/// hook scripts). Returns the status and an optional message, or None if the
/// file is empty or unrecognised.
///
/// Two accepted forms:
///
/// * Plain text: first non-empty line is a status word (see
///   [`AgentStatus::from_str`]); any remaining text (trimmed) is the message.
///   e.g. `printf 'done\nTests pass' > "$CIVET_STATUS_FILE"`.
/// * JSON object: `{"status":"needsInput","message":"Allow Bash?"}`
///   (`message` optional, other fields ignored).
///
/// Writers should replace the file atomically (write temp + rename) or just
/// overwrite it; the daemon re-reads the whole file on every change.
pub fn parse_status_file(contents: &str) -> Option<(AgentStatus, Option<String>)> {
    let text = contents.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let status = v.get("status")?.as_str()?.parse().ok()?;
        let message = v
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        return Some((status, message));
    }
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let status = first.trim().parse().ok()?;
    let rest = rest.trim();
    Some((status, (!rest.is_empty()).then(|| rest.to_owned())))
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
    fn parse_words_and_synonyms() {
        assert_eq!("needs_input".parse(), Ok(AgentStatus::NeedsInput));
        assert_eq!("Needs-Input".parse(), Ok(AgentStatus::NeedsInput));
        assert_eq!("needsInput".parse(), Ok(AgentStatus::NeedsInput));
        assert_eq!("FINISHED".parse(), Ok(AgentStatus::Done));
        assert_eq!("busy".parse(), Ok(AgentStatus::Running));
        assert!("nope".parse::<AgentStatus>().is_err());
    }

    #[test]
    fn status_file_formats() {
        assert_eq!(parse_status_file(""), None);
        assert_eq!(parse_status_file("  \n"), None);
        assert_eq!(parse_status_file("done\n"), Some((AgentStatus::Done, None)));
        assert_eq!(
            parse_status_file("needs_input\nAllow Bash(rm -rf)?\n"),
            Some((AgentStatus::NeedsInput, Some("Allow Bash(rm -rf)?".into())))
        );
        assert_eq!(
            parse_status_file(r#"{"status":"running","message":"","x":1}"#),
            Some((AgentStatus::Running, None))
        );
        assert_eq!(
            parse_status_file(r#"{"status":"done","message":"ok"}"#),
            Some((AgentStatus::Done, Some("ok".into())))
        );
        assert_eq!(parse_status_file(r#"{"status":"bogus"}"#), None);
        assert_eq!(parse_status_file("{not json"), None);
        assert_eq!(parse_status_file("garbage"), None);
    }

    #[test]
    fn attention() {
        assert!(AgentStatus::Done.wants_attention());
        assert!(AgentStatus::NeedsInput.wants_attention());
        assert!(!AgentStatus::Running.wants_attention());
    }
}
