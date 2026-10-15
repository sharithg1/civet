//! Agent status detection (pure logic; the pane feeds it signals).
//!
//! Signals, in order of trust: process exit, hook file
//! (`$CIVET_STATUS_FILE`), escape codes (OSC 9 / 777 notifications, OSC 133
//! prompt marks), heuristics (output activity, BEL then quiet, idle timeout;
//! agent panes only). After the process exited only `Client` changes apply.
//!
//! Hooks:
//!
//! * The first hook report turns any pane into an agent pane (e.g. `claude`
//!   typed in a shell pane, reporting through the PATH shim).
//! * Once a hook reported `running` or `needs_input` (a full lifecycle like
//!   Claude Code's),
//!   the activity heuristics stop applying, except two fallbacks: `Running`
//!   with no output for [`HOOK_STALL`] → `Idle` (an interrupted turn fires
//!   no `Stop` hook), and `NeedsInput` followed by user input and an output
//!   burst → `Running` (the prompt was answered).
//! * Hooks that only report the end of a turn (Codex `notify` → `done`) keep
//!   the heuristics for `Running`.
//! * OSC 9 / 777 notifications are ignored in hooked panes (hooks are more
//!   precise, and terminal-specific notifications would race them).
//!
//! Heuristics only: `Running` that goes quiet for `idle_after` becomes
//! `Done` if the run lasted at least [`SUSTAINED_RUN`], else `Idle`.

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
/// Hooked pane: Running with no output this long → Idle.
pub(crate) const HOOK_STALL: Duration = Duration::from_secs(30);
/// Heuristic-only pane: a run at least this long that goes quiet is Done.
pub(crate) const SUSTAINED_RUN: Duration = Duration::from_secs(10);
/// NeedsInput: output this soon after user input means it was answered.
const ANSWER_WINDOW: Duration = Duration::from_secs(2);

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
    /// Some hook reported (any status).
    hooked: bool,
    /// A hook reported Running or NeedsInput (a full lifecycle, not just
    /// "turn done"): heuristics are off except the fallbacks.
    hooked_running: bool,
    exited: bool,
    last_output: Option<Instant>,
    last_input: Option<Instant>,
    window_start: Option<Instant>,
    window_bytes: usize,
    bell_at: Option<Instant>,
    /// Heuristics: when the current run's activity started.
    run_start: Option<Instant>,
    /// When the status last changed.
    status_since: Instant,
    /// NeedsInput: first user input since, and output bytes after it.
    answered_at: Option<Instant>,
    answer_bytes: usize,
    /// A fallback change proposed by this tracker that `apply` lets through
    /// although heuristics are off (hooked panes).
    allow: Option<AgentStatus>,
}

impl StatusTracker {
    pub(crate) fn new(pane: PaneId, is_agent: bool, idle_after: Duration) -> Self {
        Self {
            pane,
            status: AgentStatus::Unknown,
            is_agent,
            idle_after,
            hooked: false,
            hooked_running: false,
            exited: false,
            last_output: None,
            last_input: None,
            window_start: None,
            window_bytes: 0,
            bell_at: None,
            run_start: None,
            status_since: Instant::now(),
            answered_at: None,
            answer_bytes: 0,
            allow: None,
        }
    }

    /// Apply a change if the precedence rules allow it and it differs from
    /// the current status. Returns the event to broadcast.
    pub(crate) fn apply(&mut self, change: Change) -> Option<AgentStatusEvent> {
        let allowed = self.allow.take() == Some(change.status);
        match change.source {
            AgentStatusSource::Client | AgentStatusSource::ProcessExit => {}
            _ if self.exited => return None,
            AgentStatusSource::Heuristic if self.hooked_running && !allowed => return None,
            AgentStatusSource::Hook => {
                self.hooked = true;
                self.is_agent = true;
                if matches!(
                    change.status,
                    AgentStatus::Running | AgentStatus::NeedsInput
                ) {
                    self.hooked_running = true;
                }
            }
            _ => {}
        }
        if change.source == AgentStatusSource::ProcessExit {
            self.exited = true;
        }
        // A prompt-submit hook re-reports `running` with the prompt as its
        // message (clients use it to label the session); let it through even
        // when heuristics already switched to running.
        let prompt = change.source == AgentStatusSource::Hook
            && change.status == AgentStatus::Running
            && change.message.is_some();
        if change.status == self.status && !prompt {
            return None;
        }
        let previous = std::mem::replace(&mut self.status, change.status);
        self.status_since = Instant::now();
        self.answered_at = None;
        self.answer_bytes = 0;
        if change.status != AgentStatus::Running {
            // A finished/waiting agent starts a fresh activity window.
            self.window_start = None;
            self.window_bytes = 0;
            self.run_start = None;
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

    /// A change this tracker proposes past the hook gate.
    fn fallback(&mut self, status: AgentStatus) -> Change {
        self.allow = Some(status);
        Change::new(status, AgentStatusSource::Heuristic, None)
    }

    /// Apply several changes in order, collecting events.
    pub(crate) fn apply_all(&mut self, changes: Vec<Change>) -> Vec<AgentStatusEvent> {
        changes.into_iter().filter_map(|c| self.apply(c)).collect()
    }

    /// Input was written to the PTY.
    pub(crate) fn on_input(&mut self, now: Instant) {
        self.last_input = Some(now);
        if self.status == AgentStatus::NeedsInput && self.answered_at.is_none() {
            self.answered_at = Some(now);
            self.answer_bytes = 0;
        }
    }

    /// PTY output of `len` bytes was fed; returns proposed changes in order.
    pub(crate) fn on_output(&mut self, now: Instant, len: usize, sig: &FeedSignals) -> Vec<Change> {
        let mut changes = Vec::new();
        self.last_output = Some(now);
        self.bell_at = sig.bell.then_some(now);

        // A waiting agent that got input and then redrew: it was answered.
        if self.is_agent
            && self.status == AgentStatus::NeedsInput
            && let Some(t) = self.answered_at
        {
            if now.saturating_duration_since(t) <= ANSWER_WINDOW {
                self.answer_bytes += len;
                if self.answer_bytes > ACTIVITY_BYTES {
                    self.answered_at = None;
                    changes.push(self.fallback(AgentStatus::Running));
                }
            } else {
                self.answered_at = None;
            }
        }

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
                if self.status != AgentStatus::Running || self.run_start.is_none() {
                    self.run_start = Some(self.window_start.unwrap_or(now));
                }
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
        // Hooked panes: hooks are authoritative; terminal notifications
        // (e.g. iTerm2-style OSC 9 an agent emits because of TERM_PROGRAM)
        // would race them.
        if !self.hooked {
            for body in &sig.notifications {
                let status = if notification_needs_input(body) {
                    AgentStatus::NeedsInput
                } else {
                    AgentStatus::Done
                };
                let msg = (!body.trim().is_empty()).then(|| body.trim().to_owned());
                changes.push(Change::new(status, AgentStatusSource::EscapeCode, msg));
            }
        }
        changes
    }

    /// Time-based heuristics; call periodically.
    pub(crate) fn on_tick(&mut self, now: Instant) -> Option<Change> {
        if !self.is_agent || self.exited || self.status != AgentStatus::Running {
            return None;
        }
        let last = self.last_output.unwrap_or(self.status_since);
        let quiet = now.saturating_duration_since(last);
        if self.hooked_running {
            // An interrupted turn (Esc) fires no Stop hook.
            return (quiet >= HOOK_STALL).then(|| self.fallback(AgentStatus::Idle));
        }
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
        if quiet >= self.idle_after {
            let sustained = self
                .run_start
                .is_some_and(|s| last.saturating_duration_since(s) >= SUSTAINED_RUN);
            let status = if sustained && !self.hooked {
                AgentStatus::Done
            } else {
                AgentStatus::Idle
            };
            return Some(Change::new(status, AgentStatusSource::Heuristic, None));
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

    fn hook(s: &mut StatusTracker, status: AgentStatus) -> Option<AgentStatusEvent> {
        s.apply(Change::new(status, AgentStatusSource::Hook, None))
    }

    fn tick(s: &mut StatusTracker, now: Instant) -> Option<AgentStatusEvent> {
        let c = s.on_tick(now)?;
        s.apply(c)
    }

    /// Heuristic-only agent: a long run that goes quiet is Done (M4); a
    /// short one is Idle.
    #[test]
    fn sustained_run_is_done() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(2000));
        let mut t = t0;
        while t <= t0 + ms(11_000) {
            feed(&mut s, t, 200, &FeedSignals::default());
            t += ms(500);
        }
        assert_eq!(s.status, AgentStatus::Running);
        let last = t - ms(500);
        assert!(tick(&mut s, last + ms(1000)).is_none());
        let ev = tick(&mut s, last + ms(2100)).unwrap();
        assert_eq!(ev.status, AgentStatus::Done);
        assert_eq!(ev.source, AgentStatusSource::Heuristic);
        // Next run, short: Idle.
        let t1 = last + ms(5000);
        feed(&mut s, t1, 200, &FeedSignals::default());
        assert_eq!(s.status, AgentStatus::Running);
        assert_eq!(
            tick(&mut s, t1 + ms(2100)).unwrap().status,
            AgentStatus::Idle
        );
    }

    /// Hooked (Claude-like) pane stuck on Running after an interrupt falls
    /// back to Idle after HOOK_STALL without output (M5).
    #[test]
    fn hooked_running_stall_falls_back_to_idle() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        hook(&mut s, AgentStatus::Running).unwrap();
        feed(&mut s, t0, 500, &FeedSignals::default());
        assert!(tick(&mut s, t0 + ms(10_000)).is_none(), "idle_after is off");
        let ev = tick(&mut s, t0 + HOOK_STALL + ms(10)).unwrap();
        assert_eq!(ev.status, AgentStatus::Idle);
        // Other heuristics stay off.
        feed(
            &mut s,
            t0 + HOOK_STALL + ms(100),
            500,
            &FeedSignals::default(),
        );
        assert_eq!(s.status, AgentStatus::Idle);
        // A hook brings it back.
        assert!(hook(&mut s, AgentStatus::Running).is_some());
    }

    /// NeedsInput + user input + redraw → Running (M6), even when hooked.
    #[test]
    fn answered_prompt_resumes_running() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        hook(&mut s, AgentStatus::Running).unwrap();
        hook(&mut s, AgentStatus::NeedsInput).unwrap();
        // Output without input (e.g. a spinner elsewhere) does nothing.
        feed(&mut s, t0, 500, &FeedSignals::default());
        assert_eq!(s.status, AgentStatus::NeedsInput);
        s.on_input(t0 + ms(100));
        let ev = feed(&mut s, t0 + ms(120), 300, &FeedSignals::default());
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].status, AgentStatus::Running);
        assert_eq!(ev[0].previous, AgentStatus::NeedsInput);
        // Input with only a tiny echo (e.g. focus event) does not count.
        hook(&mut s, AgentStatus::NeedsInput).unwrap();
        s.on_input(t0 + ms(1000));
        assert!(feed(&mut s, t0 + ms(1010), 10, &FeedSignals::default()).is_empty());
        assert!(feed(&mut s, t0 + ms(4000), 500, &FeedSignals::default()).is_empty());
        assert_eq!(s.status, AgentStatus::NeedsInput);
    }

    /// The first hook report makes a shell pane an agent pane; OSC 9 is
    /// then ignored (L9).
    #[test]
    fn prompt_hook_reports_while_already_running() {
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        assert!(hook(&mut s, AgentStatus::Running).is_some());
        // Same status, no message: deduplicated.
        assert!(hook(&mut s, AgentStatus::Running).is_none());
        // Same status with a prompt: passes (clients label the session).
        let ev = s
            .apply(Change::new(
                AgentStatus::Running,
                AgentStatusSource::Hook,
                Some("Fix the copay bug".into()),
            ))
            .unwrap();
        assert_eq!(ev.previous, AgentStatus::Running);
        assert_eq!(ev.message.as_deref(), Some("Fix the copay bug"));
    }

    #[test]
    fn hook_makes_agent_and_mutes_osc_notifications() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), false, ms(500));
        let osc = FeedSignals {
            notifications: vec!["Claude is waiting for your input".into()],
            ..Default::default()
        };
        assert_eq!(feed(&mut s, t0, 1, &osc)[0].status, AgentStatus::NeedsInput);
        hook(&mut s, AgentStatus::Running).unwrap();
        assert!(s.is_agent);
        assert!(feed(&mut s, t0 + ms(10), 1, &osc).is_empty());
        assert_eq!(s.status, AgentStatus::Running);
        assert!(tick(&mut s, t0 + HOOK_STALL + ms(20)).is_some());
    }

    /// A done-only hook (Codex notify) keeps the Running heuristics.
    #[test]
    fn done_only_hook_keeps_heuristics() {
        let t0 = Instant::now();
        let mut s = StatusTracker::new(PaneId(1), true, ms(500));
        feed(&mut s, t0, 200, &FeedSignals::default());
        assert_eq!(s.status, AgentStatus::Running);
        hook(&mut s, AgentStatus::Done).unwrap();
        let ev = feed(&mut s, t0 + ms(3000), 200, &FeedSignals::default());
        assert_eq!(ev[0].status, AgentStatus::Running);
        assert_eq!(
            tick(&mut s, t0 + ms(3600)).unwrap().status,
            AgentStatus::Idle
        );
    }
}
