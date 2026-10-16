//! Shared code for the `civetd` daemon and the `civet-cli` test client.
//!
//! * [`bootstrap`] (always available, also with `default-features = false`):
//!   the client side of bootstrapping a daemon over SSH (or locally),
//!   installing / upgrading the daemon binary, and the SSH stdio tunnel used
//!   when UDP is blocked.
//! * [`paths`]: state-directory layout shared by both sides.
//! * `daemon` (feature `server`, default on): the daemon itself (`serve`,
//!   server-side `bootstrap`, `stdio-tunnel`).
//! * `handoff` (feature `server`): `civetd handoff` / `install-skills`, run
//!   in panes through the `civet` shim.
//!
//! See `docs/INTERFACES.md` section 5 for the contract.

pub mod bootstrap;
#[cfg(feature = "server")]
pub mod daemon;
#[cfg(feature = "server")]
pub mod handoff;
pub mod paths;

/// `civetd` version (semver), the same for the daemon and the client crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Target triple this crate was built for (from `build.rs`).
pub const TARGET: &str = env!("CIVETD_TARGET");

/// One-line version string printed by `civetd version`.
pub fn version_line() -> String {
    format!(
        "civetd {VERSION} (proto {}, {TARGET})",
        civet_proto::PROTOCOL_VERSION
    )
}

/// JSON form printed by `civetd version --json`.
pub fn version_json() -> String {
    serde_json::json!({
        "version": VERSION,
        "proto_version": civet_proto::PROTOCOL_VERSION,
        "target": TARGET,
    })
    .to_string()
}
