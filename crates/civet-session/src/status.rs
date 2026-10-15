//! Agent status detection (pure logic; the pane feeds it signals).
//!
//! Signals, in order of trust: process exit, hook file
//! (`$CIVET_STATUS_FILE`), escape codes (OSC 9 / 777 notifications, OSC 133
//! prompt marks), heuristics (output activity, BEL then quiet, idle timeout;
//! agent panes only). Once a pane has received a Hook status, heuristics
//! stop applying to it. After the process exited only `Client` changes apply.

use std::time::{Duration, Instant};

use civet_proto::{AgentStatus, AgentStatusEvent, AgentStatusSource, PaneId};

/// Output bytes within one window that count as "agent is working".
const ACTIVITY_BYTES: usize = 64;
/// Activity window length.
const ACTIVITY_WINDOW: Duration = Duration::from_secs(1);
/// Output this soon after input is treated as echo.
const ECHO_GRACE: Duration = Duration::from_millis(150);
/// BEL followed by this much quiet → NeedsInput.
const BELL_QUIET: Duration = Duration::from_secs(1);

/// OSC 133 marks relevant to status (mirrors `civet_term::PromptMark`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    PromptStart,
    CommandStart,
    CommandExecuted,
    CommandFinished,
}

/// Status-relevant facts extracted from one `PaneTerminal::feed` call.
#[derive(Debug, Default, Clone)]
pub(crate) struct FeedSignals {
    pub bell: bool,
    /// OSC 9 / 777 notification bodies, in order.
    pub notifications: Vec<String>,
    pub marks: Vec<Mark>,
}

/// A proposed status change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    pub status: AgentStatus,
    pub source: AgentStatusSource,
    pub message: Option<String>,
}

impl Change {
    fn new(status: AgentStatus, source: AgentStatusSource, message: Option<String>) -> Self {
        Self {
            status,
            source,
            message,
        }
    }
}

/// True if a notification body looks like a question / permission prompt.
pub(crate) fn notification_needs_input(body: &str) -> bool {
    let lower = body.trim().to_lowercase();
    [
        "permission",
        "approve",
        "allow",
        "waiting",
        "input",
        "confirm",
    ]
    .iter()
    .any(|w| lower.contains(w))
        || lower.ends_with('?')
}

/// Per-pane detector + current status.
#[derive(Debug)]
pub(crate) struct StatusTracker {
    pub pane: PaneId,
    pub status: AgentStatus,
    is_agent: bool,
    idle_after: Duration,
    hooked: bool,
    exited: bool,
    last_output: Option<Instant>,
    last_input: Option<Instant>,
    window_start: Option<Instant>,
    window_bytes: usize,
    bell_at: Option<Instant>,
}

impl StatusTracker {
    pub(crate) fn new(pane: PaneId, is_agent: bool, idle_after: Duration) -> Self {
        Self {
            pane,
            status: AgentStatus::Unknown,
            is_agent,
            idle_after,
            hooked: false,
            exited: false,
            last_output: None,
            last_input: None,
            window_start: None,
            window_bytes: 0,
            bell_at: None,
        }
    }

    /// Apply a change if the precedence rules allow it and it differs from
    /// the current status. Returns the event to broadcast.
    pub(crate) fn apply(&mut self, change: Change) -> Option<AgentStatusEvent> {
        match change.source {
            AgentStatusSource::Client | AgentStatusSource::ProcessExit => {}
            _ if self.exited => return None,
            AgentStatusSource::Heuristic if self.hooked => return None,
            AgentStatusSource::Hook => self.hooked = true,
            _ => {}
        }
        if change.source == AgentStatusSource::ProcessExit {
            self.exited = true;
        }
        if change.status == self.status {
            return None;
        }
        let previous = std::mem::replace(&mut self.status, change.status);
        if change.status != AgentStatus::Running {
            // A finished/waiting agent starts a fresh activity window.
            self.window_start = None;
            self.window_bytes = 0;
        }
        Some(AgentStatusEvent {
            pane: self.pane,
            status: change.status,
            previous,
            source: change.source,
            message: change.message,
            at_ms: civet_proto::now_ms(),
        })
    }

    /// Apply several changes in order, collecting events.
    pub(crate) fn apply_all(&mut self, changes: Vec<Change>) -> Vec<AgentStatusEvent> {
        changes.into_iter().filter_map(|c| self.apply(c)).collect()
    }

    /// Input was written to the PTY.
    pub(crate) fn on_input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// PTY output of `len` bytes was fed; returns proposed changes in order.
    pub(crate) fn on_output(&mut self, now: Instant, len: usize, sig: &FeedSignals) -> Vec<Change> {
        let mut changes = Vec::new();
        self.last_output = Some(now);
        self.bell_at = sig.bell.then_some(now);

        let echo = self
            .last_input
            .is_some_and(|t| now.saturating_duration_since(t) < ECHO_GRACE);
        if self.is_agent && !echo {
            let fresh = self
                .window_start
                .is_none_or(|t| now.saturating_duration_since(t) > ACTIVITY_WINDOW);
            if fresh {
                self.window_start = Some(now);
                self.window_bytes = 0;
            }
            self.window_bytes += len;
            if self.window_bytes > ACTIVITY_BYTES {
                changes.push(Change::new(
                    AgentStatus::Running,
                    AgentStatusSource::Heuristic,
                    None,
                ));
            }
        }

        for mark in &sig.marks {
            match mark {
                Mark::CommandExecuted => changes.push(Change::new(
                    AgentStatus::Running,
                    AgentStatusSource::EscapeCode,
                    None,
                )),
                Mark::CommandFinished | Mark::PromptStart if !self.is_agent => changes.push(
                    Change::new(AgentStatus::Idle, AgentStatusSource::EscapeCode, None),
                ),
                _ => {}
            }
        }
        for body in &sig.notifications {
            let status = if notification_needs_input(body) {
                AgentStatus::NeedsInput
            } else {
                AgentStatus::Done
            };
            let msg = (!body.trim().is_empty()).then(|| body.trim().to_owned());
            changes.push(Change::new(status, AgentStatusSource::EscapeCode, msg));
        }
        changes
    }

    /// Time-based heuristics; call periodically.
    pub(crate) fn on_tick(&mut self, now: Instant) -> Option<Change> {
        if !self.is_agent || self.hooked || self.exited || self.status != AgentStatus::Running {
            return None;
        }
        let last = self.last_output?;
        let quiet_after_bell = self
            .bell_at
            .is_some_and(|bell| last <= bell && now.saturating_duration_since(bell) >= BELL_QUIET);
        if quiet_after_bell {
            self.bell_at = None;
            return Some(Change::new(
                AgentStatus::NeedsInput,
                AgentStatusSource::Heuristic,
                None,
            ));
        }
        if now.saturating_duration_since(last) >= self.idle_after {
            return Some(Change::new(
                AgentStatus::Idle,
                AgentStatusSource::Heuristic,
                None,
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn needs_input_words() {
        assert!(notification_needs_input(
            "Claude needs your permission to use Bash"
        ));
        assert!(notification_needs_input("Proceed?"));
        assert!(notification_needs_input("Waiting for input"));
        assert!(!notification_needs_input("Task complete"));
    }

    #[test]
    fn activity_then_idle() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        let ev = feed(&mut s, t0, 10, &FeedSignals::default());
        assert!(ev.is_empty(), "10 bytes is not activity");
        let ev = feed(&mut s, t0 + ms(10), 100, &FeedSignals::default());
        assert_eq!(ev[0].status, AgentStatus::Running);
        assert_eq!(ev[0].source, AgentStatusSource::Heuristic);
        assert!(s.on_tick(t0 + ms(300)).is_none());
        let c = s.on_tick(t0 + ms(600)).unwrap();
        assert_eq!(c.status, AgentStatus::Idle);
    }

    fn s_on(s: &mut StatusTracker, now: Instant, len: usize) -> Vec<Change> {
        s.on_output(now, len, &FeedSignals::default())
    }

    fn feed(
        s: &mut StatusTracker,
        now: Instant,
        len: usize,
        sig: &FeedSignals,
    ) -> Vec<AgentStatusEvent> {
        let changes = s.on_output(now, len, sig);
        s.apply_all(changes)
    }

    #[test]
    fn echo_is_ignored() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        s.on_input(t0);
        assert!(s_on(&mut s, t0 + ms(20), 500).is_empty());
    }

    #[test]
    fn bell_then_quiet() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, Duration::from_secs(60));
        feed(&mut s, t0, 100, &FeedSignals::default());
        let sig = FeedSignals {
            bell: true,
            ..Default::default()
        };
        s.on_output(t0 + ms(10), 1, &sig);
        assert!(s.on_tick(t0 + ms(500)).is_none());
        assert_eq!(
            s.on_tick(t0 + ms(1100)).unwrap().status,
            AgentStatus::NeedsInput
        );
    }

    #[test]
    fn osc_and_hook_precedence() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), false, ms(500));
        // non-agent: activity does nothing; 133;C → running; 133;D → idle
        assert!(s_on(&mut s, t0, 1000).is_empty());
        let sig = FeedSignals {
            marks: vec![Mark::CommandExecuted],
            ..Default::default()
        };
        assert_eq!(feed(&mut s, t0, 1, &sig)[0].status, AgentStatus::Running);
        let sig = FeedSignals {
            marks: vec![Mark::CommandFinished],
            ..Default::default()
        };
        assert_eq!(feed(&mut s, t0, 1, &sig)[0].status, AgentStatus::Idle);
        let sig = FeedSignals {
            notifications: vec!["Allow edit?".into()],
            ..Default::default()
        };
        let ev = feed(&mut s, t0, 1, &sig);
        assert_eq!(ev[0].status, AgentStatus::NeedsInput);
        assert_eq!(ev[0].message.as_deref(), Some("Allow edit?"));

        // hooks disable heuristics
        let mut a = StatusTracker::new(PaneId(2), true, ms(100));
        let hook = Change::new(AgentStatus::Running, AgentStatusSource::Hook, None);
        assert!(a.apply(hook).is_some());
        a.last_output = Some(t0);
        assert!(a.on_tick(t0 + ms(500)).is_none());
        let heur = Change::new(AgentStatus::Idle, AgentStatusSource::Heuristic, None);
        assert!(a.apply(heur).is_none());
        // exit wins and is final
        let exit = Change::new(AgentStatus::Done, AgentStatusSource::ProcessExit, None);
        assert_eq!(a.apply(exit).unwrap().previous, AgentStatus::Running);
        let osc = Change::new(AgentStatus::Running, AgentStatusSource::EscapeCode, None);
        assert!(a.apply(osc).is_none());
        let client = Change::new(AgentStatus::Idle, AgentStatusSource::Client, None);
        assert!(a.apply(client).is_some());
    }
}
