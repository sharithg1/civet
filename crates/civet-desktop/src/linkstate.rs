//! Maps the session's `LinkState` to the UI's `HostState`
//! (docs/INTERFACES.md §6.3): `"reconnecting"` is shown only if recovery
//! lasts longer than [`RECONNECTING_AFTER`], so sub-second resumes (wake,
//! Wi-Fi switch) stay invisible.

use std::time::Duration;

use civet_transport::{LinkState, REPLACED_REASON, TransportKind};
use tokio::time::Instant;

use crate::events::HostState;

/// Delay before the UI shows "reconnecting".
pub const RECONNECTING_AFTER: Duration = Duration::from_secs(1);

/// `failed` message when another client with this app's client id (a second
/// Civet instance, or a copied `~/.civet/client-id-desktop`) took the
/// daemon connection over twice in a row.
pub const REPLACED_MESSAGE: &str = "another Civet window or client took over this host's connection; \
     reconnect here to take it back";

/// The UI text for a `LinkState::Failed` reason.
pub fn failure_message(reason: &str) -> String {
    if reason == REPLACED_REASON {
        REPLACED_MESSAGE.to_string()
    } else {
        reason.to_string()
    }
}

/// A state to publish (host id added by the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStateUpdate {
    /// State.
    pub state: HostState,
    /// Link kind.
    pub transport: Option<TransportKind>,
    /// RTT.
    pub rtt_ms: Option<u32>,
    /// Reason / error.
    pub message: Option<String>,
}

/// What the host task should do after a link change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// Publish this state.
    Publish(HostStateUpdate),
    /// The daemon needs a fresh bootstrap (restarted / said goodbye).
    Rebootstrap {
        /// Why.
        reason: String,
    },
}

/// The state machine. Pure: the caller supplies the clock and runs the
/// timer ([`LinkMapper::deadline`] → [`LinkMapper::on_timer`]).
#[derive(Debug, Clone)]
pub struct LinkMapper {
    shown: HostState,
    transport: Option<TransportKind>,
    rtt_ms: Option<u32>,
    recovering_since: Option<Instant>,
    reason: Option<String>,
    threshold: Duration,
}

impl LinkMapper {
    /// Starting in `Connecting`.
    pub fn new(threshold: Duration) -> Self {
        Self {
            shown: HostState::Connecting,
            transport: None,
            rtt_ms: None,
            recovering_since: None,
            reason: None,
            threshold,
        }
    }

    /// Starting in `Connected` (a session that just finished its first dial).
    pub fn connected(threshold: Duration, transport: TransportKind) -> Self {
        Self {
            shown: HostState::Connected,
            transport: Some(transport),
            ..Self::new(threshold)
        }
    }

    /// State currently shown.
    pub fn shown(&self) -> HostState {
        self.shown
    }

    /// Current state as an update.
    pub fn current(&self) -> HostStateUpdate {
        HostStateUpdate {
            state: self.shown,
            transport: match self.shown {
                HostState::Connected | HostState::Reconnecting => self.transport,
                _ => None,
            },
            rtt_ms: self.rtt_ms,
            message: match self.shown {
                HostState::Connected => None,
                _ => self.reason.clone(),
            },
        }
    }

    fn show(&mut self, state: HostState) -> HostStateUpdate {
        self.shown = state;
        self.current()
    }

    /// Feed a link state change.
    pub fn on_link(&mut self, s: &LinkState, now: Instant) -> Option<LinkAction> {
        match s {
            LinkState::Connecting => (self.shown != HostState::Connecting)
                .then(|| LinkAction::Publish(self.show(HostState::Connecting))),
            LinkState::Connected { transport, rtt_ms } => {
                let changed = self.shown != HostState::Connected
                    || self.transport != Some(*transport)
                    || self.rtt_ms != *rtt_ms;
                self.recovering_since = None;
                self.reason = None;
                self.transport = Some(*transport);
                self.rtt_ms = *rtt_ms;
                changed.then(|| LinkAction::Publish(self.show(HostState::Connected)))
            }
            LinkState::Recovering { reason, .. } => {
                self.begin_recovery(reason.clone(), now);
                None
            }
            LinkState::Failed {
                reason,
                needs_bootstrap,
            } => {
                if *needs_bootstrap {
                    self.begin_recovery(reason.clone(), now);
                    Some(LinkAction::Rebootstrap {
                        reason: reason.clone(),
                    })
                } else {
                    Some(LinkAction::Publish(self.fail(failure_message(reason))))
                }
            }
            LinkState::Closed => {
                self.recovering_since = None;
                (self.shown != HostState::Disconnected)
                    .then(|| LinkAction::Publish(self.show(HostState::Disconnected)))
            }
        }
    }

    /// Recovery started (link stall, re-bootstrap). The timer starts only
    /// once per outage.
    pub fn begin_recovery(&mut self, reason: String, now: Instant) {
        if self.recovering_since.is_none() {
            self.recovering_since = Some(now);
        }
        self.reason = Some(reason);
    }

    /// Give up.
    pub fn fail(&mut self, reason: String) -> HostStateUpdate {
        self.recovering_since = None;
        self.reason = Some(reason);
        self.show(HostState::Failed)
    }

    /// Back to connected after a re-bootstrap. The RTT is forgotten (the
    /// new session measures its own) so its first sample is published.
    pub fn reconnected(&mut self, transport: TransportKind) -> HostStateUpdate {
        self.recovering_since = None;
        self.reason = None;
        self.rtt_ms = None;
        self.transport = Some(transport);
        self.show(HostState::Connected)
    }

    /// When to call [`Self::on_timer`].
    pub fn deadline(&self) -> Option<Instant> {
        match self.shown {
            HostState::Reconnecting => None,
            _ => self.recovering_since.map(|t| t + self.threshold),
        }
    }

    /// Timer fired: show "reconnecting" if recovery is still running.
    pub fn on_timer(&mut self, now: Instant) -> Option<HostStateUpdate> {
        let since = self.recovering_since?;
        (self.shown != HostState::Reconnecting && now >= since + self.threshold)
            .then(|| self.show(HostState::Reconnecting))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn connected(rtt: u32) -> LinkState {
        LinkState::Connected {
            transport: TransportKind::Quic,
            rtt_ms: Some(rtt),
        }
    }

    fn recovering() -> LinkState {
        LinkState::Recovering {
            since_ms: 0,
            reason: "stall".into(),
        }
    }

    fn published(a: Option<LinkAction>) -> HostState {
        match a {
            Some(LinkAction::Publish(u)) => u.state,
            other => panic!("expected publish, got {other:?}"),
        }
    }

    #[test]
    fn short_recovery_is_invisible() {
        let t0 = Instant::now();
        let mut m = LinkMapper::new(RECONNECTING_AFTER);
        assert_eq!(
            published(m.on_link(&connected(20), t0)),
            HostState::Connected
        );
        assert_eq!(m.on_link(&recovering(), t0), None);
        assert_eq!(m.deadline(), Some(t0 + RECONNECTING_AFTER));
        assert_eq!(m.on_timer(t0 + 999 * MS), None);
        // Resumed before the threshold, same transport and RTT: nothing.
        assert_eq!(m.on_link(&connected(20), t0 + 600 * MS), None);
        assert_eq!(m.deadline(), None);
        assert_eq!(m.on_timer(t0 + 2000 * MS), None);
        assert_eq!(m.shown(), HostState::Connected);
    }

    #[test]
    fn reconnecting_only_after_one_second_then_connected() {
        let t0 = Instant::now();
        let mut m = LinkMapper::connected(RECONNECTING_AFTER, TransportKind::Quic);
        m.on_link(&recovering(), t0);
        // A second Recovering does not restart the timer.
        m.on_link(&recovering(), t0 + 500 * MS);
        assert_eq!(m.deadline(), Some(t0 + RECONNECTING_AFTER));
        assert_eq!(m.on_timer(t0 + 500 * MS), None);
        let u = m.on_timer(t0 + 1000 * MS).expect("reconnecting");
        assert_eq!(u.state, HostState::Reconnecting);
        assert_eq!(u.message.as_deref(), Some("stall"));
        assert_eq!(m.deadline(), None, "fires once");
        assert_eq!(
            published(m.on_link(&connected(25), t0 + 1500 * MS)),
            HostState::Connected
        );
        // RTT changes are published while connected.
        assert_eq!(
            published(m.on_link(&connected(40), t0 + 1600 * MS)),
            HostState::Connected
        );
    }

    #[test]
    fn failures_and_close() {
        let t0 = Instant::now();
        let mut m = LinkMapper::connected(RECONNECTING_AFTER, TransportKind::Stream);
        let a = m.on_link(
            &LinkState::Failed {
                reason: "bad key".into(),
                needs_bootstrap: true,
            },
            t0,
        );
        assert_eq!(
            a,
            Some(LinkAction::Rebootstrap {
                reason: "bad key".into()
            })
        );
        // Re-bootstrap running: reconnecting after the threshold.
        assert_eq!(
            m.on_timer(t0 + RECONNECTING_AFTER).unwrap().state,
            HostState::Reconnecting
        );
        assert_eq!(
            m.reconnected(TransportKind::Quic).state,
            HostState::Connected
        );
        let a = m.on_link(
            &LinkState::Failed {
                reason: "version".into(),
                needs_bootstrap: false,
            },
            t0,
        );
        assert_eq!(published(a), HostState::Failed);
        assert_eq!(
            published(m.on_link(&LinkState::Closed, t0)),
            HostState::Disconnected
        );
        assert_eq!(m.on_link(&LinkState::Closed, t0), None);
    }

    #[test]
    fn rtt_is_republished_after_a_rebootstrap() {
        let t0 = Instant::now();
        let mut m = LinkMapper::connected(RECONNECTING_AFTER, TransportKind::Quic);
        assert_eq!(
            published(m.on_link(&connected(80), t0)),
            HostState::Connected
        );
        // The re-bootstrap installs a new session (published with no RTT);
        // the new session's first sample (the same 80 ms) must be published
        // again or the host keeps rtt None and prediction stays off.
        m.begin_recovery("restart".into(), t0);
        let u = m.reconnected(TransportKind::Quic);
        assert_eq!(u.rtt_ms, None);
        match m.on_link(&connected(80), t0) {
            Some(LinkAction::Publish(u)) => assert_eq!(u.rtt_ms, Some(80)),
            other => panic!("expected an RTT publish, got {other:?}"),
        }
    }

    #[test]
    fn replaced_has_a_clear_message() {
        let mut m = LinkMapper::connected(RECONNECTING_AFTER, TransportKind::Quic);
        let a = m.on_link(
            &LinkState::Failed {
                reason: REPLACED_REASON.into(),
                needs_bootstrap: false,
            },
            Instant::now(),
        );
        match a {
            Some(LinkAction::Publish(u)) => {
                assert_eq!(u.state, HostState::Failed);
                assert_eq!(u.message.as_deref(), Some(REPLACED_MESSAGE));
            }
            other => panic!("{other:?}"),
        }
    }
}
