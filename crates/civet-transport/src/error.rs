//! Transport error type and conversions from quinn errors.

use civet_proto::{CertFingerprint, HelloReject, ProtoError, RejectReason};

/// Everything that can go wrong moving frames between client and daemon.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Socket / file I/O.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Framing or (de)serialization.
    #[error("protocol: {0}")]
    Proto(#[from] ProtoError),
    /// quinn connect/connection/write/read errors, stringified so quinn types
    /// don't leak into the public API.
    #[error("quic: {0}")]
    Quic(String),
    /// TLS configuration or certificate problem.
    #[error("tls/certificate: {0}")]
    Tls(String),
    /// The daemon presented a certificate other than the pinned one.
    #[error("certificate fingerprint mismatch: expected {expected}, got {actual}")]
    FingerprintMismatch {
        /// Pinned fingerprint (from the bootstrap line).
        expected: CertFingerprint,
        /// Fingerprint of the certificate the peer presented.
        actual: CertFingerprint,
    },
    /// Daemon refused the hello. `BadKey` => daemon restarted: re-bootstrap.
    #[error("hello rejected: {0:?}")]
    Rejected(HelloReject),
    /// An operation did not finish in time.
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// The peer sent something the protocol does not allow.
    #[error("peer violated protocol: {0}")]
    ProtocolViolation(String),
    /// Connection closed (cleanly or not). For ClientConnection: the link is
    /// gone; ClientSession reconnects on its own.
    #[error("connection closed: {0}")]
    Closed(String),
    /// The daemon closed this connection because a newer connection with the
    /// same `ClientId` took over (QUIC close code 3). Redialing would just
    /// kick the other connection off in turn; `ClientSession` stops with
    /// `LinkState::Failed { reason: REPLACED_REASON, needs_bootstrap: false }`
    /// unless the newer connection is its own (racing) dial.
    #[error("connection taken over by another connection of this client: {0}")]
    Replaced(String),
}

impl TransportError {
    /// True if retrying the same dial cannot help (Rejected with
    /// BadKey/VersionMismatch/BadMagic, FingerprintMismatch).
    pub fn is_fatal(&self) -> bool {
        match self {
            TransportError::Rejected(r) => matches!(
                r.reason,
                RejectReason::BadKey | RejectReason::VersionMismatch | RejectReason::BadMagic
            ),
            TransportError::FingerprintMismatch { .. } => true,
            _ => false,
        }
    }
}

/// Application close codes (INTERFACES.md 2.3).
pub mod close_code {
    /// Normal close (after `Goodbye`).
    pub const NORMAL: u32 = 0;
    /// Hello rejected; the reason bytes carry the postcard-encoded
    /// `HelloReject`.
    pub const REJECTED: u32 = 1;
    /// Peer violated the protocol.
    pub const PROTOCOL_VIOLATION: u32 = 2;
    /// Replaced by a newer connection of the same client.
    pub const REPLACED: u32 = 3;
    /// Daemon shutting down.
    pub const SHUTTING_DOWN: u32 = 4;
}

/// Map a quinn connection error into a `TransportError`.
pub(crate) fn from_conn_error(e: &quinn::ConnectionError) -> TransportError {
    use quinn::ConnectionError as E;
    match e {
        E::ApplicationClosed(c) => {
            let code = c.error_code.into_inner();
            if code == u64::from(close_code::REJECTED)
                && let Ok(reject) = civet_proto::decode::<HelloReject>(&c.reason)
            {
                return TransportError::Rejected(reject);
            }
            if code == u64::from(close_code::REPLACED) {
                return TransportError::Replaced(String::from_utf8_lossy(&c.reason).into_owned());
            }
            TransportError::Closed(format!(
                "closed by peer (code {code}): {}",
                String::from_utf8_lossy(&c.reason)
            ))
        }
        E::LocallyClosed => TransportError::Closed("closed locally".into()),
        E::TimedOut => TransportError::Closed("idle timeout".into()),
        E::Reset => TransportError::Closed("reset by peer (stateless reset)".into()),
        E::ConnectionClosed(c) => TransportError::Closed(format!("aborted by peer: {c}")),
        other => TransportError::Quic(other.to_string()),
    }
}

/// Map an error on a QUIC stream, preferring the connection's close reason
/// when the connection is gone (stream errors only say "connection lost").
pub(crate) fn stream_error(conn: &quinn::Connection, e: impl std::fmt::Display) -> TransportError {
    match conn.close_reason() {
        Some(reason) => from_conn_error(&reason),
        None => TransportError::Quic(e.to_string()),
    }
}

/// Like [`stream_error`] for a framing error from `read_raw_frame`.
pub(crate) fn frame_error(conn: &quinn::Connection, e: ProtoError) -> TransportError {
    match (&e, conn.close_reason()) {
        (ProtoError::Io(_) | ProtoError::Truncated(_), Some(reason)) => from_conn_error(&reason),
        _ => TransportError::Proto(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_classification() {
        assert!(TransportError::Rejected(HelloReject::new(RejectReason::BadKey, "x")).is_fatal());
        assert!(
            TransportError::Rejected(HelloReject::new(RejectReason::VersionMismatch, "x"))
                .is_fatal()
        );
        assert!(
            !TransportError::Rejected(HelloReject::new(RejectReason::ShuttingDown, "x")).is_fatal()
        );
        assert!(
            TransportError::FingerprintMismatch {
                expected: CertFingerprint([0; 32]),
                actual: CertFingerprint([1; 32]),
            }
            .is_fatal()
        );
        assert!(!TransportError::Timeout("x").is_fatal());
        assert!(!TransportError::Closed("x".into()).is_fatal());
    }
}
