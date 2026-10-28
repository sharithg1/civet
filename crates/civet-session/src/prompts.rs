//! Prompts typed into panes for the user (see `civet_proto::prompts`): the
//! bytes that deliver one, and the per-pane queues the status ticker fires.
//!
//! Delivery: the text goes in the way a paste would, wrapped in bracketed
//! paste markers when the program turned that mode on (Claude Code and
//! Codex do), with newlines as CR like xterm.js pastes them; then, after
//! [`SUBMIT_DELAY`], a lone CR presses Enter. Why two writes: both TUIs
//! treat a CR that arrives in the same burst as pasted text as part of the
//! paste (a newline in the input box), not as "submit".
//!
//! Queues: each pane's prompts go out oldest first. The ticker watches the
//! pane's agent status; when it changes into a status the first prompt's
//! trigger fires on, the queue is armed, and if the status still holds
//! [`FIRE_SETTLE`] later the prompt is typed and submitted. The settle time
//! lets the TUI get back to its input box after the hook reported the end
//! of the turn, and skips a "done" that is immediately followed by more
//! work.
//!
//! One prompt at a time per pane: while a pane's Enter is pending, a prompt
//! sent to it now waits ([`PromptQueues::defer`]) and a queued one is
//! retried, so two texts never land before one Enter (which would submit
//! them as one message).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use civet_proto::{
    AgentStatus, MAX_PROMPT_BYTES, MAX_QUEUED_PROMPTS, PaneId, PromptTrigger, QueuedPrompt,
};

use crate::SessionError;

/// Time between typing a prompt and pressing Enter.
pub(crate) const SUBMIT_DELAY: Duration = Duration::from_millis(400);
/// A trigger status must hold this long before a queued prompt is sent.
pub(crate) const FIRE_SETTLE: Duration = Duration::from_millis(750);
/// After a refused write (PTY backlog full), try again this much later.
pub(crate) const FIRE_RETRY: Duration = Duration::from_secs(1);
/// What "press Enter" writes.
pub(crate) const SUBMIT: &[u8] = b"\r";

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Validate and normalize prompt text: line endings become `\n`, control
/// characters other than newline and tab are dropped (an ESC could end the
/// bracketed paste early and turn the rest into keystrokes), trailing
/// whitespace is trimmed (the submit is a separate Enter).
pub(crate) fn clean_prompt(text: &str) -> Result<String, SessionError> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned: String = text
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    let cleaned = cleaned.trim_end();
    if cleaned.trim().is_empty() {
        return Err(SessionError::InvalidArgument("empty prompt".into()));
    }
    if cleaned.len() > MAX_PROMPT_BYTES {
        return Err(SessionError::InvalidArgument(format!(
            "prompt is {} bytes; at most {MAX_PROMPT_BYTES}",
            cleaned.len()
        )));
    }
    Ok(cleaned.to_owned())
}

/// The bytes that type `text` (already [`clean_prompt`]ed), without the
/// Enter: a bracketed paste if the program enabled it, else the text as is.
pub(crate) fn prompt_bytes(text: &str, bracketed_paste: bool) -> Vec<u8> {
    let body = text.replace('\n', "\r");
    if !bracketed_paste {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(PASTE_END);
    out
}

/// Every pane's queued prompts plus the timers that send them.
#[derive(Debug, Default)]
pub(crate) struct PromptQueues {
    next_id: u64,
    queues: BTreeMap<PaneId, Vec<QueuedPrompt>>,
    /// Status of each pane with a queue when the ticker last looked (a
    /// queue fires on a change into a trigger status, not on one it was
    /// queued in).
    seen: HashMap<PaneId, AgentStatus>,
    /// Pane → when its first prompt is due.
    armed: HashMap<PaneId, Instant>,
    /// Enter presses still to come: (pane, due).
    submits: Vec<(PaneId, Instant)>,
    /// Prompts sent now to a pane whose Enter was still pending, in order.
    deferred: Vec<(PaneId, String)>,
}

impl PromptQueues {
    /// Add a prompt at the end of `pane`'s queue; `status` is the pane's
    /// status now (a later change into a trigger status fires it).
    pub(crate) fn push(
        &mut self,
        pane: PaneId,
        status: AgentStatus,
        text: String,
        trigger: PromptTrigger,
    ) -> Result<QueuedPrompt, SessionError> {
        let queue = self.queues.entry(pane).or_default();
        if queue.len() >= MAX_QUEUED_PROMPTS {
            return Err(SessionError::InvalidArgument(format!(
                "{pane} already has {MAX_QUEUED_PROMPTS} queued prompts"
            )));
        }
        self.next_id += 1;
        let prompt = QueuedPrompt {
            id: self.next_id,
            pane,
            text,
            trigger,
            created_at_ms: civet_proto::now_ms(),
        };
        queue.push(prompt.clone());
        self.seen.entry(pane).or_insert(status);
        Ok(prompt)
    }

    /// Change a prompt's text and trigger in place.
    pub(crate) fn edit(
        &mut self,
        id: u64,
        text: String,
        trigger: PromptTrigger,
    ) -> Result<QueuedPrompt, SessionError> {
        let p = self
            .queues
            .values_mut()
            .flatten()
            .find(|p| p.id == id)
            .ok_or_else(|| SessionError::NotFound(format!("queued prompt {id}")))?;
        p.text = text;
        p.trigger = trigger;
        Ok(p.clone())
    }

    /// Remove a prompt (cancelled or sent); returns it.
    pub(crate) fn remove(&mut self, id: u64) -> Option<QueuedPrompt> {
        let (&pane, queue) = self
            .queues
            .iter_mut()
            .find(|(_, q)| q.iter().any(|p| p.id == id))?;
        let i = queue.iter().position(|p| p.id == id)?;
        let p = queue.remove(i);
        if queue.is_empty() {
            self.forget(pane);
        }
        Some(p)
    }

    /// Drop a pane's queue (it exited or closed). True if it had one.
    pub(crate) fn drop_pane(&mut self, pane: PaneId) -> bool {
        let had = self.queues.contains_key(&pane);
        self.forget(pane);
        self.submits.retain(|(p, _)| *p != pane);
        self.deferred.retain(|(p, _)| *p != pane);
        had
    }

    fn forget(&mut self, pane: PaneId) {
        self.queues.remove(&pane);
        self.seen.remove(&pane);
        self.armed.remove(&pane);
    }

    /// `pane`'s queue, oldest first.
    pub(crate) fn of(&self, pane: PaneId) -> Vec<QueuedPrompt> {
        self.queues.get(&pane).cloned().unwrap_or_default()
    }

    /// Every queued prompt, by pane, oldest first.
    pub(crate) fn all(&self) -> Vec<QueuedPrompt> {
        self.queues.values().flatten().cloned().collect()
    }

    /// Panes that have a queue.
    pub(crate) fn panes(&self) -> Vec<PaneId> {
        self.queues.keys().copied().collect()
    }

    /// The ticker saw `pane` in `status`: a change into a status the first
    /// prompt fires on arms the queue (once; a second trigger status keeps
    /// the first due time), a change into any other disarms it.
    pub(crate) fn observe(&mut self, pane: PaneId, status: AgentStatus, now: Instant) {
        let Some(head) = self.queues.get(&pane).and_then(|q| q.first()) else {
            return;
        };
        let prev = self.seen.insert(pane, status);
        if prev == Some(status) {
            return;
        }
        if head.trigger.fires_on(status) {
            self.armed.entry(pane).or_insert(now + FIRE_SETTLE);
        } else {
            self.armed.remove(&pane);
        }
    }

    /// The first prompt of every armed pane whose time has come (disarmed;
    /// the caller sends it, or calls [`Self::retry`]). Only prompts whose
    /// trigger still fires on `status(pane)` are returned.
    pub(crate) fn due(
        &mut self,
        now: Instant,
        status: impl Fn(PaneId) -> Option<AgentStatus>,
    ) -> Vec<QueuedPrompt> {
        let ready: Vec<PaneId> = self
            .armed
            .iter()
            .filter(|&(_, &at)| at <= now)
            .map(|(&pane, _)| pane)
            .collect();
        let mut out = Vec::new();
        for pane in ready {
            self.armed.remove(&pane);
            let Some(head) = self.queues.get(&pane).and_then(|q| q.first()) else {
                continue;
            };
            if status(pane).is_some_and(|s| head.trigger.fires_on(s)) {
                out.push(head.clone());
            }
        }
        out
    }

    /// Re-arm `pane` (its write was refused) for `at`.
    pub(crate) fn retry(&mut self, pane: PaneId, at: Instant) {
        if self.queues.contains_key(&pane) {
            self.armed.insert(pane, at);
        }
    }

    /// True while `pane` has a typed prompt whose Enter is still to come:
    /// nothing else may be typed into it until then.
    pub(crate) fn submitting(&self, pane: PaneId) -> bool {
        self.submits.iter().any(|&(p, _)| p == pane)
    }

    /// Type `text` into `pane` once its pending Enter is in.
    pub(crate) fn defer(&mut self, pane: PaneId, text: String) {
        self.deferred.push((pane, text));
    }

    /// The oldest deferred prompt of each pane that is free now (left in
    /// place; [`Self::take_deferred`] once it was typed).
    pub(crate) fn ready_deferred(&self) -> Vec<(PaneId, String)> {
        let mut out: Vec<(PaneId, String)> = Vec::new();
        for (pane, text) in &self.deferred {
            if !self.submitting(*pane) && !out.iter().any(|(p, _)| p == pane) {
                out.push((*pane, text.clone()));
            }
        }
        out
    }

    /// Drop `pane`'s oldest deferred prompt (it was typed).
    pub(crate) fn take_deferred(&mut self, pane: PaneId) {
        if let Some(i) = self.deferred.iter().position(|(p, _)| *p == pane) {
            self.deferred.remove(i);
        }
    }

    /// Press Enter in `pane` at `at`.
    pub(crate) fn submit_at(&mut self, pane: PaneId, at: Instant) {
        self.submits.push((pane, at));
    }

    /// Panes whose Enter is due now, in order.
    pub(crate) fn due_submits(&mut self, now: Instant) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.submits.retain(|&(pane, at)| {
            if at <= now {
                out.push(pane);
                false
            } else {
                true
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: PaneId = PaneId(1);

    #[test]
    fn clean() {
        assert_eq!(clean_prompt("fix it\r\n\n").unwrap(), "fix it");
        assert_eq!(clean_prompt("a\r\nb\rc").unwrap(), "a\nb\nc");
        // ESC and other controls dropped (no early paste end), tab kept.
        assert_eq!(clean_prompt("x\x1b[201~y\x07\tz").unwrap(), "x[201~y\tz");
        assert_eq!(clean_prompt("  indented").unwrap(), "  indented");
        assert!(clean_prompt(" \n\t\r").is_err());
        assert!(clean_prompt("\x1b").is_err());
        assert!(clean_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert!(clean_prompt(&"x".repeat(MAX_PROMPT_BYTES)).is_ok());
    }

    #[test]
    fn bytes() {
        assert_eq!(prompt_bytes("hi", false), b"hi");
        assert_eq!(prompt_bytes("a\nb", false), b"a\rb");
        assert_eq!(prompt_bytes("a\nb", true), b"\x1b[200~a\rb\x1b[201~");
        // Never contains the Enter itself.
        assert!(!prompt_bytes("one line", true).ends_with(SUBMIT));
    }

    fn queues_with(status: AgentStatus, trigger: PromptTrigger) -> PromptQueues {
        let mut q = PromptQueues::default();
        q.push(P, status, "next".into(), trigger).unwrap();
        q
    }

    #[test]
    fn fires_on_a_change_into_the_trigger_status_after_settling() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Running, PromptTrigger::Done);
        let status = |_| Some(AgentStatus::Done);
        q.observe(P, AgentStatus::Running, t0);
        assert!(q.due(t0 + FIRE_SETTLE * 2, status).is_empty());
        q.observe(P, AgentStatus::Done, t0);
        assert!(q.due(t0 + FIRE_SETTLE / 2, status).is_empty());
        let due = q.due(t0 + FIRE_SETTLE, status);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "next");
        // Disarmed: the caller sends it (or retries); not returned twice.
        assert!(q.due(t0 + FIRE_SETTLE * 3, status).is_empty());
    }

    #[test]
    fn already_in_the_trigger_status_waits_for_the_next_change() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Done, PromptTrigger::Done);
        q.observe(P, AgentStatus::Done, t0);
        assert!(
            q.due(t0 + FIRE_SETTLE, |_| Some(AgentStatus::Done))
                .is_empty()
        );
        q.observe(P, AgentStatus::Running, t0 + FIRE_SETTLE);
        q.observe(P, AgentStatus::Done, t0 + FIRE_SETTLE * 2);
        assert_eq!(
            q.due(t0 + FIRE_SETTLE * 3, |_| Some(AgentStatus::Done))
                .len(),
            1
        );
    }

    #[test]
    fn a_brief_done_does_not_fire() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Running, PromptTrigger::Done);
        q.observe(P, AgentStatus::Done, t0);
        q.observe(P, AgentStatus::Running, t0 + FIRE_SETTLE / 2);
        assert!(
            q.due(t0 + FIRE_SETTLE, |_| Some(AgentStatus::Running))
                .is_empty()
        );
        // Status changed under the timer without the ticker seeing it: the
        // due check looks again.
        q.observe(P, AgentStatus::Done, t0 + FIRE_SETTLE);
        assert!(
            q.due(t0 + FIRE_SETTLE * 2, |_| Some(AgentStatus::Running))
                .is_empty()
        );
    }

    #[test]
    fn needs_input_only_fires_the_matching_trigger() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Running, PromptTrigger::Done);
        q.observe(P, AgentStatus::NeedsInput, t0);
        let needs = |_| Some(AgentStatus::NeedsInput);
        assert!(q.due(t0 + FIRE_SETTLE, needs).is_empty());

        let mut q = queues_with(AgentStatus::Running, PromptTrigger::DoneOrNeedsInput);
        q.observe(P, AgentStatus::NeedsInput, t0);
        // Done right after keeps the first due time.
        q.observe(P, AgentStatus::Done, t0 + FIRE_SETTLE / 2);
        assert_eq!(q.due(t0 + FIRE_SETTLE, needs).len(), 1);
    }

    #[test]
    fn queue_order_edit_cancel() {
        let mut q = PromptQueues::default();
        let a = q
            .push(P, AgentStatus::Running, "a".into(), PromptTrigger::Done)
            .unwrap();
        let b = q
            .push(P, AgentStatus::Running, "b".into(), PromptTrigger::Done)
            .unwrap();
        let c = q
            .push(
                PaneId(2),
                AgentStatus::Idle,
                "c".into(),
                PromptTrigger::Done,
            )
            .unwrap();
        assert!(a.id < b.id && b.id < c.id);
        assert_eq!(
            q.of(P).iter().map(|p| &p.text).collect::<Vec<_>>(),
            ["a", "b"]
        );
        let e = q
            .edit(b.id, "B".into(), PromptTrigger::DoneOrNeedsInput)
            .unwrap();
        assert_eq!(
            (e.text.as_str(), e.trigger),
            ("B", PromptTrigger::DoneOrNeedsInput)
        );
        assert_eq!(q.remove(a.id).unwrap().text, "a");
        assert_eq!(q.of(P)[0].text, "B");
        assert!(q.remove(a.id).is_none());
        assert!(q.edit(a.id, "x".into(), PromptTrigger::Done).is_err());
        assert_eq!(q.all().len(), 2);
        assert_eq!(q.panes(), [P, PaneId(2)]);
        assert!(q.drop_pane(PaneId(2)));
        assert!(!q.drop_pane(PaneId(2)));
        assert_eq!(q.all().len(), 1);
        // Removing the last prompt forgets the pane.
        q.remove(b.id).unwrap();
        assert!(q.panes().is_empty());
    }

    #[test]
    fn queue_limit() {
        let mut q = PromptQueues::default();
        for i in 0..MAX_QUEUED_PROMPTS {
            q.push(P, AgentStatus::Running, i.to_string(), PromptTrigger::Done)
                .unwrap();
        }
        assert!(
            q.push(P, AgentStatus::Running, "x".into(), PromptTrigger::Done)
                .is_err()
        );
    }

    #[test]
    fn next_prompt_waits_for_the_next_turn() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Running, PromptTrigger::Done);
        q.push(
            P,
            AgentStatus::Running,
            "second".into(),
            PromptTrigger::Done,
        )
        .unwrap();
        q.observe(P, AgentStatus::Done, t0);
        let done = |_| Some(AgentStatus::Done);
        let first = q.due(t0 + FIRE_SETTLE, done).remove(0);
        q.remove(first.id);
        // Still "done" right after the send: nothing more fires.
        q.observe(P, AgentStatus::Done, t0 + FIRE_SETTLE);
        assert!(q.due(t0 + FIRE_SETTLE * 3, done).is_empty());
        q.observe(P, AgentStatus::Running, t0 + FIRE_SETTLE * 4);
        q.observe(P, AgentStatus::Done, t0 + FIRE_SETTLE * 5);
        assert_eq!(q.due(t0 + FIRE_SETTLE * 6, done)[0].text, "second");
    }

    #[test]
    fn retry_and_submits() {
        let t0 = Instant::now();
        let mut q = queues_with(AgentStatus::Running, PromptTrigger::Done);
        q.retry(P, t0);
        assert_eq!(q.due(t0, |_| Some(AgentStatus::Done)).len(), 1);
        q.retry(PaneId(9), t0);
        assert!(q.due(t0, |_| Some(AgentStatus::Done)).is_empty());

        q.submit_at(P, t0 + SUBMIT_DELAY);
        q.submit_at(PaneId(2), t0);
        assert_eq!(q.due_submits(t0), [PaneId(2)]);
        assert_eq!(q.due_submits(t0 + SUBMIT_DELAY), [P]);
        assert!(q.due_submits(t0 + SUBMIT_DELAY).is_empty());
        // A busy pane's Enter is retried later (the ticker re-adds it).
        q.submit_at(P, t0 + FIRE_RETRY);
        assert!(q.due_submits(t0).is_empty());
        assert_eq!(q.due_submits(t0 + FIRE_RETRY), [P]);
        q.submit_at(P, t0);
        q.drop_pane(P);
        assert!(q.due_submits(t0).is_empty());
    }

    #[test]
    fn one_prompt_at_a_time_per_pane() {
        let t0 = Instant::now();
        let mut q = PromptQueues::default();
        q.submit_at(P, t0 + SUBMIT_DELAY);
        assert!(q.submitting(P));
        assert!(!q.submitting(PaneId(2)));
        q.defer(P, "a".into());
        q.defer(PaneId(2), "x".into());
        q.defer(P, "b".into());
        // P still waits for its Enter; pane 2 is free.
        assert_eq!(q.ready_deferred(), [(PaneId(2), "x".to_owned())]);
        q.take_deferred(PaneId(2));
        assert_eq!(q.due_submits(t0 + SUBMIT_DELAY), [P]);
        assert!(!q.submitting(P));
        // Oldest first, one per pane per round.
        assert_eq!(q.ready_deferred(), [(P, "a".to_owned())]);
        q.take_deferred(P);
        q.submit_at(P, t0 + SUBMIT_DELAY * 2);
        assert!(q.ready_deferred().is_empty());
        q.drop_pane(P);
        assert!(q.ready_deferred().is_empty());
    }
}
