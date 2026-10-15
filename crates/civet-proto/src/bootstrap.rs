//! SSH bootstrap handshake.
//!
//! The client runs `civetd` over SSH (`ssh host civetd bootstrap ...`).
//! The daemon starts (or finds an already-running instance), then prints
//! exactly one line of JSON to stdout:
//!
//! ```json
//! {"civet_bootstrap":1,"daemon_version":"0.1.0","proto_version":1,"udp_port":60001,
//!  "session_key":"<64 hex>","cert_fingerprint":"<64 hex>","pid":1234,"reattached":false}
//! ```
//!
//! The client parses it with [`BootstrapInfo::find_in_output`] (which skips
//! login banners / MOTD noise), closes SSH and dials QUIC to `udp_port`.
//!
//! Field names are snake_case and fixed; new optional fields may be added
//! (JSON is self-describing), so unknown fields are ignored when parsing.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;
use crate::keys::{CertFingerprint, SessionKey};

/// Value of the `civet_bootstrap` marker field. Bumped if the line's meaning
/// changes incompatibly.
pub const BOOTSTRAP_FORMAT: u32 = 1;

/// Everything the client needs to dial the daemon over QUIC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapInfo {
    /// Marker so the line can be found among other output; always
    /// [`BOOTSTRAP_FORMAT`].
    pub civet_bootstrap: u32,
    /// `civetd` version string (semver), e.g. `"0.1.0"`.
    pub daemon_version: String,
    /// Wire protocol version the daemon speaks ([`crate::PROTOCOL_VERSION`]).
    pub proto_version: u32,
    /// UDP port the daemon's QUIC endpoint listens on.
    pub udp_port: u16,
    /// Secret the client must present in its [`crate::ClientHello`].
    pub session_key: SessionKey,
    /// SHA-256 of the daemon's self-signed certificate (DER), for pinning.
    pub cert_fingerprint: CertFingerprint,
    /// Daemon process id (diagnostics / upgrade handling).
    #[serde(default)]
    pub pid: u32,
    /// True if an already-running daemon was reused rather than started.
    #[serde(default)]
    pub reattached: bool,
    /// Bootstrap output only: the version of the `civetd` binary that ran
    /// the bootstrap (the newest one installed). Differs from
    /// `daemon_version` when an older daemon was kept running.
    #[serde(default)]
    pub available_version: Option<String>,
    /// Bootstrap output only: number of live panes of the daemon (when the
    /// running daemon was reattached).
    #[serde(default)]
    pub live_panes: Option<u32>,
    /// Bootstrap output only: a newer `civetd` (`available_version`) is
    /// installed but the running daemon was kept because it has live panes.
    /// Bootstrapping again with `replace` restarts it (killing the panes).
    #[serde(default)]
    pub upgrade_available: bool,
}

impl BootstrapInfo {
    /// Build a bootstrap record for this build's protocol version.
    pub fn new(
        daemon_version: impl Into<String>,
        udp_port: u16,
        session_key: SessionKey,
        cert_fingerprint: CertFingerprint,
    ) -> Self {
        Self {
            civet_bootstrap: BOOTSTRAP_FORMAT,
            daemon_version: daemon_version.into(),
            proto_version: crate::PROTOCOL_VERSION,
            udp_port,
            session_key,
            cert_fingerprint,
            pid: std::process::id(),
            reattached: false,
            available_version: None,
            live_panes: None,
            upgrade_available: false,
        }
    }

    /// Serialize as a single JSON line terminated by `\n`.
    pub fn to_json_line(&self) -> String {
        // Serializing this struct cannot fail: only strings/ints/bools.
        let mut s = serde_json::to_string(self).expect("BootstrapInfo is always serializable");
        s.push('\n');
        s
    }

    /// Parse exactly one JSON line.
    pub fn from_json_line(line: &str) -> Result<Self, ProtoError> {
        Ok(serde_json::from_str(line.trim())?)
    }

    /// Scan arbitrary command output (MOTD, shell noise, warnings) and return
    /// the **last** line that is a valid bootstrap record.
    pub fn find_in_output(output: &str) -> Result<Self, ProtoError> {
        output
            .lines()
            .rev()
            .map(str::trim)
            .filter(|l| l.starts_with('{') && l.contains("\"civet_bootstrap\""))
            .find_map(|l| Self::from_json_line(l).ok())
            .ok_or(ProtoError::NoBootstrapLine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BootstrapInfo {
        BootstrapInfo::new(
            "0.1.0",
            60001,
            SessionKey([1; 32]),
            CertFingerprint([2; 32]),
        )
    }

    #[test]
    fn json_line_roundtrip() {
        let b = sample();
        let line = b.to_json_line();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.contains("\"civet_bootstrap\":1"));
        assert!(line.contains(&"01".repeat(32)));
        assert_eq!(BootstrapInfo::from_json_line(&line).unwrap(), b);
    }

    #[test]
    fn upgrade_fields_are_optional_and_roundtrip() {
        let mut b = sample();
        b.available_version = Some("0.2.0".into());
        b.live_panes = Some(3);
        b.upgrade_available = true;
        let line = b.to_json_line();
        assert!(line.contains("\"upgrade_available\":true"));
        assert_eq!(BootstrapInfo::from_json_line(&line).unwrap(), b);
    }

    #[test]
    fn find_in_noisy_output() {
        let b = sample();
        let out = format!(
            "Welcome to Ubuntu\n{{\"not\":\"it\"}}\nLast login: yesterday\n{}warning: foo\n",
            b.to_json_line()
        );
        assert_eq!(BootstrapInfo::find_in_output(&out).unwrap(), b);
        assert!(matches!(
            BootstrapInfo::find_in_output("nothing here\n"),
            Err(ProtoError::NoBootstrapLine)
        ));
    }

    #[test]
    fn ignores_unknown_fields_and_defaults_optional() {
        let line = format!(
            "{{\"civet_bootstrap\":1,\"daemon_version\":\"9.9.9\",\"proto_version\":1,\
             \"udp_port\":1,\"session_key\":\"{}\",\"cert_fingerprint\":\"{}\",\"future\":true}}",
            "00".repeat(32),
            "ff".repeat(32)
        );
        let b = BootstrapInfo::from_json_line(&line).unwrap();
        assert_eq!(b.pid, 0);
        assert!(!b.reattached);
        assert_eq!(b.available_version, None);
        assert_eq!(b.live_panes, None);
        assert!(!b.upgrade_available);
        assert_eq!(b.cert_fingerprint, CertFingerprint([0xff; 32]));
    }
}
