//! Per-(client, pane) screen subscription with mosh-style flow control.
//!
//! * **Newest wins**: every update is encoded from the baseline (last sent
//!   snapshot) to the pane's *current* state at the moment of sending.
//! * **Coalescing**: after a change wakes an idle subscription it waits
//!   [`IDLE_COALESCE`] for more output; under continuous output it sends at
//!   most one update per [`BUSY_INTERVAL`].
//! * **Bounded window**: at most [`MAX_INFLIGHT_UPDATES`] unacked updates;
//!   while the window is full nothing is sent (changes keep coalescing).
//! * **Echo acks**: `acked_input_seq` covers input written at least
//!   `ECHO_ACK_DELAY_MS` before the snapshot; input acked that way without a
//!   screen update to carry it is reported as [`PaneOutput::InputAck`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use civet_proto::{ClientId, MAX_INFLIGHT_UPDATES, PaneId, ScreenUpdate, SubscribeInfo};
use civet_term::Snapshot;
use tokio::sync::Notify;

use crate::pane::{Pane, lock};

/// Wait for more output after a change on an idle pane.
pub const IDLE_COALESCE: Duration = Duration::from_millis(2);
/// Minimum spacing of updates under continuous output.
pub const BUSY_INTERVAL: Duration = Duration::from_millis(8);

/// What a subscription's send task should transmit next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneOutput {
    /// A screen update (send on the pane's screen stream).
    Screen(ScreenUpdate),
    /// Input applied >= `ECHO_ACK_DELAY_MS` ago without a screen update that
    /// carried its ack (send on the control stream).
    InputAck {
        /// Pane.
        pane: PaneId,
        /// Highest echo-acked input seq.
        seq: u64,
    },
}

#[derive(Default)]
struct SubState {
    /// Snapshot the next diff is relative to (None → Full).
    baseline: Option<Arc<Snapshot>>,
    /// Sent but not yet acked.
    inflight: VecDeque<Arc<Snapshot>>,
    /// Newest snapshot the client acked.
    acked: Option<Arc<Snapshot>>,
    /// Send an update even if nothing changed (after reset_baseline).
    force: bool,
    /// When the current unsent change was first noticed.
    pending_since: Option<Instant>,
    last_sent_at: Option<Instant>,
    /// Highest input seq already reported (in an update or an InputAck).
    reported_input_seq: u64,
}

struct Inner {
    client: ClientId,
    pane: Arc<Pane>,
    state: Mutex<SubState>,
    notify: Notify,
}

/// Handle shared between the daemon's per-pane send task and its control
/// message handler. Clone = same subscription. All methods take `&self`.
#[derive(Clone)]
pub struct Subscription {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = lock(&self.inner.state);
        f.debug_struct("Subscription")
            .field("client", &self.inner.client)
            .field("pane", &self.inner.pane.id)
            .field("baseline", &st.baseline.as_ref().map(|s| s.state()))
            .field("inflight", &st.inflight.len())
            .field("acked", &st.acked.as_ref().map(|s| s.state()))
            .finish()
    }
}

enum Step {
    Output(PaneOutput),
    Closed,
    Wait(Option<Instant>),
}

impl Subscription {
    pub(crate) fn new(client: ClientId, pane: Arc<Pane>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                pane,
                state: Mutex::new(SubState {
                    force: true,
                    ..Default::default()
                }),
                notify: Notify::new(),
            }),
        }
    }

    /// Pane id.
    pub fn pane(&self) -> PaneId {
        self.inner.pane.id
    }

    /// Client id.
    pub fn client(&self) -> ClientId {
        self.inner.client
    }

    /// Current SubscribeInfo (state, size, history_lines, last_input_seq).
    pub fn info(&self) -> SubscribeInfo {
        let pane = &self.inner.pane;
        let (state, size, history_lines) = {
            let term = lock(&pane.term);
            (term.state(), term.size(), term.history_lines())
        };
        SubscribeInfo {
            pane: pane.id,
            state,
            size,
            history_lines,
            last_input_seq: pane.last_input_seq(self.inner.client),
        }
    }

    /// Number of updates sent but not yet acked.
    pub fn inflight(&self) -> usize {
        lock(&self.inner.state).inflight.len()
    }

    /// Set the diff baseline: the snapshot for `known_state` if this
    /// subscription still holds it (last acked, or in flight), else none
    /// (next update is Full). Clears the in-flight queue. The next call to
    /// [`Subscription::next`] returns an update right away (possibly an
    /// empty Diff).
    pub fn reset_baseline(&self, known_state: Option<u64>) {
        let mut st = lock(&self.inner.state);
        let found = known_state.and_then(|s| {
            st.acked
                .iter()
                .chain(st.inflight.iter())
                .find(|snap| snap.state() == s)
                .cloned()
        });
        if found.is_some() {
            // The client says it shows this state: it is effectively acked.
            st.acked = found.clone();
        }
        st.baseline = found;
        st.inflight.clear();
        st.force = true;
        st.pending_since = None;
        st.last_sent_at = None;
        // A new connection must learn the ack again.
        st.reported_input_seq = 0;
        drop(st);
        self.inner.notify.notify_one();
    }

    /// Client applied `state`: release in-flight snapshots <= state; the
    /// acked snapshot becomes the resume baseline.
    pub fn ack(&self, state: u64) {
        let mut st = lock(&self.inner.state);
        let mut newest = None;
        while st.inflight.front().is_some_and(|s| s.state() <= state) {
            newest = st.inflight.pop_front();
        }
        if let Some(snap) = newest {
            if st.acked.as_ref().is_none_or(|a| snap.state() > a.state()) {
                st.acked = Some(snap);
            }
        }
        drop(st);
        self.inner.notify.notify_one();
    }

    /// Wait for the next thing to send. Returns None when the pane is closed.
    /// Waits while the in-flight window is full; coalesces; always encodes
    /// from the baseline to the *current* state (newest wins). Cancel-safe:
    /// state is only mutated right before returning.
    pub async fn next(&self) -> Option<PaneOutput> {
        let pane = &self.inner.pane;
        let mut state_rx = pane.state_tx.subscribe();
        let mut input_rx = pane.input_tx.subscribe();
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            state_rx.borrow_and_update();
            input_rx.borrow_and_update();
            let deadline = match self.step(Instant::now()) {
                Step::Output(out) => return Some(out),
                Step::Closed => return None,
                Step::Wait(deadline) => deadline,
            };
            let sleep = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = &mut notified => {}
                r = state_rx.changed() => { if r.is_err() { return None; } }
                r = input_rx.changed() => { if r.is_err() { return None; } }
                _ = sleep => {}
            }
        }
    }

    /// One non-blocking evaluation of what to send.
    fn step(&self, now: Instant) -> Step {
        let pane = &self.inner.pane;
        if pane.is_closed() {
            return Step::Closed;
        }
        let current = *pane.state_tx.borrow();
        let (acked_seq, ack_deadline) = pane.echo_ack(self.inner.client, now);
        let mut st = lock(&self.inner.state);
        let base_state = st.baseline.as_ref().map(|s| s.state());
        let dirty = st.force || base_state != Some(current);
        let mut wake: Option<Instant> = ack_deadline;
        if dirty && st.inflight.len() < MAX_INFLIGHT_UPDATES {
            let since = *st.pending_since.get_or_insert(now);
            let due = if st.force {
                now
            } else {
                match st.last_sent_at {
                    Some(last) if now.saturating_duration_since(last) < BUSY_INTERVAL => {
                        (last + BUSY_INTERVAL).max(since + IDLE_COALESCE)
                    }
                    _ => since + IDLE_COALESCE,
                }
            };
            if now >= due {
                let (encoded, snap) = {
                    let mut term = lock(&pane.term);
                    let encoded = term.encode_update(st.baseline.as_deref());
                    let snap = term.snapshot();
                    (encoded, snap)
                };
                st.inflight.push_back(snap.clone());
                st.baseline = Some(snap);
                st.force = false;
                st.pending_since = None;
                st.last_sent_at = Some(now);
                st.reported_input_seq = st.reported_input_seq.max(acked_seq);
                return Step::Output(PaneOutput::Screen(
                    encoded.into_screen_update(pane.id, acked_seq),
                ));
            }
            wake = Some(wake.map_or(due, |w| w.min(due)));
        }
        if acked_seq > st.reported_input_seq {
            st.reported_input_seq = acked_seq;
            return Step::Output(PaneOutput::InputAck {
                pane: pane.id,
                seq: acked_seq,
            });
        }
        Step::Wait(wake)
    }
}
