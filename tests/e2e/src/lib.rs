//! Hardening / end-to-end harness for Civet (milestone "Hardening", weeks
//! 7–8 of the plan).
//!
//! * [`proxy::UdpProxy`]: an in-test UDP impairment proxy between a QUIC
//!   client and the daemon: loss, latency + jitter, reordering, blackhole,
//!   client roaming and NAT rebinding.
//! * [`harness`]: in-process and real-binary daemons in temp state dirs,
//!   dialers (QUIC, real `civetd stdio-tunnel`), a `ClientSession` wrapper
//!   with per-pane mirrors, and a raw connection for resume-token tests.
//! * [`mirror::Mirror`]: the client's copy of a pane (what xterm.js would
//!   show) plus a server-numbered transcript to prove nothing was lost.
//! * [`metrics`]: `METRIC {json}` lines (and `$CIVET_E2E_METRICS`).
//!
//! The scenarios are in `tests/`; see the crate README section in the
//! repository root README for how to run them.

pub mod harness;
pub mod metrics;
pub mod mirror;
pub mod proxy;

pub use harness::*;
pub use metrics::{Summary, env_num, ms, record, record_summary};
pub use mirror::{Mirror, assert_contiguous, strip_vt, trim_rows};
pub use proxy::{Impairment, UdpProxy};
