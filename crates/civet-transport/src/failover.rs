//! A [`Dialer`] that fails over from a preferred link (QUIC) to a fallback
//! (SSH stdio tunnel) when the preferred one stops working mid-session, and
//! moves back once it works again.
//!
//! * Not in trouble: dial the primary (bounded by `primary_timeout`). After
//!   `fail_over_after_attempts` consecutive non-fatal failures, or when one
//!   attempt took `fail_over_after` (e.g. UDP silently dropped), dial the
//!   fallback in the same call.
//! * The session reports link trouble ([`Dialer::link_trouble`]); once it
//!   has lasted `fail_over_after`, dials race primary and fallback and keep
//!   whichever completes its hello first.
//! * On the fallback, dials go to the fallback. The session calls
//!   [`Dialer::dial_preferred`] every probe interval (via
//!   [`Dialer::upgrade_interval`]). Moving back has hysteresis, so a link
//!   that works one moment and stalls the next does not make the session
//!   swap links all the time:
//!   * Each call first only *probes* the primary ([`Dialer::probe`]: for
//!     QUIC a handshake without a hello, which leaves the session's
//!     connection alone) and returns an error. Only the call whose probe is
//!     the `probes_to_return`-th success in a row goes on to dial; a failed
//!     probe or dial starts the count again. When that dial succeeds the
//!     session adopts the connection (resume) and drops the fallback one.
//!   * A move back that is followed by another failover within
//!     `flap_window` did not hold: the probe interval doubles (up to
//!     `max_probe_interval`). A failover after a longer stay on the primary
//!     resets it to `probe_interval`.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use civet_proto::{ClientHello, ClientMessage};
use futures::future::BoxFuture;

use crate::conn::{Connected, TransportKind};
use crate::error::TransportError;
use crate::session::Dialer;

/// Failover timings.
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// Upper bound for one primary dial. Default 3 s.
    pub primary_timeout: Duration,
    /// Fail over after this many consecutive failed primary dials.
    /// Default 2.
    pub fail_over_after_attempts: u32,
    /// Fail over once trouble (or a single primary attempt) has lasted this
    /// long. Default 3 s.
    pub fail_over_after: Duration,
    /// While on the fallback, probe the primary this often (doubled after
    /// each move back that did not hold, see `flap_window`). Default 10 s.
    pub probe_interval: Duration,
    /// Consecutive successful probes of the primary (each within
    /// `primary_timeout`) before the session moves back to it; 0 = dial it
    /// without probing. Default 3: with the default interval a session
    /// stays on the fallback for at least 30 s.
    pub probes_to_return: u32,
    /// A failover this soon after moving back to the primary doubles the
    /// probe interval; a later one resets it. Default 5 min.
    pub flap_window: Duration,
    /// Upper bound of the doubled probe interval. Default 2 min (at most
    /// 6 min on the fallback between attempts to move back).
    pub max_probe_interval: Duration,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            primary_timeout: Duration::from_secs(3),
            fail_over_after_attempts: 2,
            fail_over_after: Duration::from_secs(3),
            probe_interval: Duration::from_secs(10),
            probes_to_return: 3,
            flap_window: Duration::from_secs(300),
            max_probe_interval: Duration::from_secs(120),
        }
    }
}

#[derive(Debug)]
struct State {
    on_fallback: bool,
    primary_failures: u32,
    trouble_since: Option<Instant>,
    /// Successful probes of the primary in a row (while on the fallback).
    probes_ok: u32,
    /// Current (backed-off) probe interval.
    probe_interval: Duration,
    /// When the session last moved back from the fallback to the primary.
    returned_at: Option<Instant>,
}

/// Primary-with-fallback dialer (see the module docs).
pub struct FailoverDialer {
    primary: Arc<dyn Dialer>,
    fallback: Arc<dyn Dialer>,
    config: FailoverConfig,
    state: Mutex<State>,
}

impl fmt::Debug for FailoverDialer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.lock();
        f.debug_struct("FailoverDialer")
            .field("primary", &self.primary.kind())
            .field("fallback", &self.fallback.kind())
            .field("on_fallback", &st.on_fallback)
            .field("primary_failures", &st.primary_failures)
            .finish()
    }
}

fn is_final(e: &TransportError) -> bool {
    e.is_fatal() || matches!(e, TransportError::Rejected(_))
}

impl FailoverDialer {
    /// `primary` is preferred (QUIC); `fallback` is used while it fails.
    pub fn new(
        primary: Arc<dyn Dialer>,
        fallback: Arc<dyn Dialer>,
        config: FailoverConfig,
    ) -> Self {
        let state = State {
            on_fallback: false,
            primary_failures: 0,
            trouble_since: None,
            probes_ok: 0,
            probe_interval: config.probe_interval,
            returned_at: None,
        };
        Self {
            primary,
            fallback,
            config,
            state: Mutex::new(state),
        }
    }

    /// True while dials go to the fallback.
    pub fn on_fallback(&self) -> bool {
        self.lock().on_fallback
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn dial_primary(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> Result<Connected, TransportError> {
        let r = match tokio::time::timeout(
            self.config.primary_timeout,
            self.primary.dial(hello, pipelined),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(TransportError::Timeout("primary link dial")),
        };
        let mut st = self.lock();
        match &r {
            Ok(_) => {
                if st.on_fallback {
                    tracing::info!("preferred link works again; leaving the fallback");
                    st.returned_at = Some(Instant::now());
                }
                st.on_fallback = false;
                st.primary_failures = 0;
            }
            Err(e) if !is_final(e) => st.primary_failures += 1,
            Err(_) => {}
        }
        st.probes_ok = 0;
        r
    }

    async fn dial_fallback(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> Result<Connected, TransportError> {
        let r = self.fallback.dial(hello, pipelined).await;
        if r.is_ok() {
            let mut st = self.lock();
            if !st.on_fallback {
                // Back here soon after the last move to the primary: that
                // move did not hold, so wait longer before the next one.
                let flapped = st
                    .returned_at
                    .is_some_and(|t| t.elapsed() < self.config.flap_window);
                st.probe_interval = if flapped {
                    (st.probe_interval * 2)
                        .min(self.config.max_probe_interval)
                        .max(self.config.probe_interval)
                } else {
                    self.config.probe_interval
                };
                st.probes_ok = 0;
                tracing::info!(
                    flapped,
                    probe_interval_ms = st.probe_interval.as_millis() as u64,
                    "preferred link unavailable; using the fallback"
                );
            }
            st.on_fallback = true;
        }
        r
    }

    /// One probe of the primary on behalf of [`Dialer::dial_preferred`].
    /// Ok: the primary has now answered `probes_to_return` probes in a row,
    /// go on and dial it.
    async fn prove_primary(&self) -> Result<(), TransportError> {
        let need = self.config.probes_to_return;
        if need == 0 {
            return Ok(());
        }
        let r = match tokio::time::timeout(self.config.primary_timeout, self.primary.probe()).await
        {
            Ok(r) => r,
            Err(_) => Err(TransportError::Timeout("primary link probe")),
        };
        let mut st = self.lock();
        match r {
            Ok(took) => {
                st.probes_ok += 1;
                tracing::debug!(
                    took_ms = took.as_millis() as u64,
                    ok = st.probes_ok,
                    need,
                    "preferred link answered a probe"
                );
                if st.probes_ok >= need {
                    Ok(())
                } else {
                    Err(TransportError::Timeout(
                        "preferred link is still proving itself",
                    ))
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "preferred link probe failed");
                st.probes_ok = 0;
                Err(e)
            }
        }
    }

    /// Dial both; the first successful hello wins.
    async fn race(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> Result<Connected, TransportError> {
        let p = self.dial_primary(hello.clone(), pipelined.clone());
        let f = self.dial_fallback(hello, pipelined);
        tokio::pin!(p, f);
        let mut p_err: Option<TransportError> = None;
        let mut f_err: Option<TransportError> = None;
        loop {
            tokio::select! {
                r = &mut p, if p_err.is_none() => match r {
                    Ok(c) => return Ok(c),
                    Err(e) if is_final(&e) => return Err(e),
                    Err(e) => p_err = Some(e),
                },
                r = &mut f, if f_err.is_none() => match r {
                    Ok(c) => return Ok(c),
                    Err(e) => f_err = Some(e),
                },
            }
            if let (Some(_), Some(_)) = (&p_err, &f_err) {
                return Err(p_err.expect("checked"));
            }
        }
    }
}

impl Dialer for FailoverDialer {
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        Box::pin(async move {
            let (on_fallback, trouble) = {
                let st = self.lock();
                let trouble = st.primary_failures >= self.config.fail_over_after_attempts
                    || st
                        .trouble_since
                        .is_some_and(|t| t.elapsed() >= self.config.fail_over_after);
                (st.on_fallback, trouble)
            };
            if on_fallback {
                return self.dial_fallback(hello, pipelined).await;
            }
            if trouble {
                return self.race(hello, pipelined).await;
            }
            let started = Instant::now();
            match self.dial_primary(hello.clone(), pipelined.clone()).await {
                Ok(c) => Ok(c),
                Err(e) if is_final(&e) => Err(e),
                Err(e) => {
                    let failures = self.lock().primary_failures;
                    if failures >= self.config.fail_over_after_attempts
                        || started.elapsed() >= self.config.fail_over_after
                    {
                        tracing::info!(error = %e, failures, "primary link failed; trying the fallback");
                        self.dial_fallback(hello, pipelined).await
                    } else {
                        Err(e)
                    }
                }
            }
        })
    }

    fn rebind(&self) -> std::io::Result<()> {
        let r = self.primary.rebind();
        if let Err(e) = self.fallback.rebind() {
            tracing::debug!(error = %e, "fallback rebind failed");
        }
        r
    }

    fn kind(&self) -> TransportKind {
        self.primary.kind()
    }

    fn link_trouble(&self, since: Option<std::time::Instant>) {
        self.lock().trouble_since = since;
        self.primary.link_trouble(since);
        self.fallback.link_trouble(since);
    }

    fn upgrade_interval(&self) -> Option<Duration> {
        Some(self.lock().probe_interval)
    }

    fn dial_preferred(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> BoxFuture<'_, Result<Connected, TransportError>> {
        Box::pin(async move {
            self.prove_primary().await?;
            self.dial_primary(hello, pipelined).await
        })
    }

    fn probe(&self) -> BoxFuture<'_, Result<Duration, TransportError>> {
        self.primary.probe()
    }
}
