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
/// overwrite it; the daemon re-reads the whole file on every change (at most
/// [`MAX_STATUS_FILE_BYTES`] of it).
///
/// The message is sanitized: control characters other than newline and tab
/// are removed and it is truncated to [`MAX_STATUS_MESSAGE_CHARS`] characters
/// (an ellipsis marks the cut), so a misbehaving hook cannot produce an
/// oversized or terminal-corrupting `AgentStatus` event.
pub fn parse_status_file(contents: &str) -> Option<(AgentStatus, Option<String>)> {
    let text = contents.trim();
    if text.is_empty() {
        return None;
    }
    let (status, message) = if text.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let status = v.get("status")?.as_str()?.parse().ok()?;
        let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
        (status, sanitize_status_message(message))
    } else {
        let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
        let status = first.trim().parse().ok()?;
        (status, sanitize_status_message(rest))
    };
    Some((status, message))
}

/// Daemons read at most this many bytes of a pane's status file.
pub const MAX_STATUS_FILE_BYTES: u64 = 4096;

/// Status-file messages are truncated to this many characters.
pub const MAX_STATUS_MESSAGE_CHARS: usize = 500;

/// Strip control characters (keeping `\n` and `\t`), trim, truncate to
/// [`MAX_STATUS_MESSAGE_CHARS`]. None if nothing is left.
fn sanitize_status_message(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars();
    let mut out: String = chars.by_ref().take(MAX_STATUS_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        // Keep the total at MAX_STATUS_MESSAGE_CHARS including the ellipsis.
        out.pop();
        let keep = out.trim_end().len();
        out.truncate(keep);
        out.push('\u{2026}');
    }
    Some(out)
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
    fn status_file_message_sanitized() {
        // Control characters (ESC, BEL, CR, NUL) are stripped; newline and
        // tab survive.
        assert_eq!(
            parse_status_file("done\n\x1b[31mred\x07\r\0 ok\n\tnext"),
            Some((AgentStatus::Done, Some("[31mred ok\n\tnext".into())))
        );
        assert_eq!(
            parse_status_file(r#"{"status":"done","message":"\u001b\u0007"}"#),
            Some((AgentStatus::Done, None))
        );
        // Long messages are truncated to MAX_STATUS_MESSAGE_CHARS chars.
        let long = format!("needs_input\n{}", "\u{e9}".repeat(10_000));
        let (status, msg) = parse_status_file(&long).unwrap();
        assert_eq!(status, AgentStatus::NeedsInput);
        let msg = msg.unwrap();
        assert_eq!(msg.chars().count(), MAX_STATUS_MESSAGE_CHARS);
        assert!(msg.ends_with('\u{2026}'));
        let json = format!(r#"{{"status":"done","message":"{}"}}"#, "x".repeat(5000));
        let msg = parse_status_file(&json).unwrap().1.unwrap();
        assert_eq!(msg.chars().count(), MAX_STATUS_MESSAGE_CHARS);
        // Exactly at the limit: untouched.
        let exact = format!("done\n{}", "y".repeat(MAX_STATUS_MESSAGE_CHARS));
        let msg = parse_status_file(&exact).unwrap().1.unwrap();
        assert_eq!(msg, "y".repeat(MAX_STATUS_MESSAGE_CHARS));
    }

    #[test]
    fn attention() {
        assert!(AgentStatus::Done.wants_attention());
        assert!(AgentStatus::NeedsInput.wants_attention());
        assert!(!AgentStatus::Running.wants_attention());
    }
}
