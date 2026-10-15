//! Connection handshake: the first frame in each direction on the control
//! stream.
//!
//! The QUIC/TLS handshake has already pinned the daemon's certificate, so the
//! session key travels inside an authenticated, encrypted channel. The daemon
//! compares it in constant time and answers with [`HelloReply`].

use serde::{Deserialize, Serialize};

use crate::MAGIC;
use crate::error::ProtoError;
use crate::ids::ClientId;
use crate::keys::SessionKey;

/// First frame the client sends on the control stream.
///
/// **Field order of `magic` and `proto_version` is frozen forever** so that
/// [`peek_hello_version`] works across protocol versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    /// Always [`MAGIC`].
    pub magic: [u8; 4],
    /// Client's [`crate::PROTOCOL_VERSION`].
    pub proto_version: u32,
    /// Secret from [`crate::BootstrapInfo::session_key`].
    pub session_key: SessionKey,
    /// Stable per-installation id; keys the daemon's per-client resume state.
    pub client_id: ClientId,
    /// Human-readable client name, e.g. `"civet-desktop"` or `"civet-cli"`.
    pub client_name: String,
    /// Client build version (semver).
    pub client_version: String,
}

impl ClientHello {
    /// Hello for this build's protocol version.
    pub fn new(
        session_key: SessionKey,
        client_id: ClientId,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Self {
        Self {
            magic: MAGIC,
            proto_version: crate::PROTOCOL_VERSION,
            session_key,
            client_id,
            client_name: client_name.into(),
            client_version: client_version.into(),
        }
    }

    /// Daemon-side validation: magic, version and key (constant time).
    /// Returns the rejection to send, if any.
    pub fn validate(&self, expected_key: &SessionKey) -> Result<(), HelloReject> {
        if self.magic != MAGIC {
            return Err(HelloReject::new(RejectReason::BadMagic, "bad magic"));
        }
        if !crate::is_compatible(self.proto_version) {
            return Err(HelloReject::new(
                RejectReason::VersionMismatch,
                format!(
                    "client protocol v{} not supported (daemon supports v{}..=v{})",
                    self.proto_version,
                    crate::MIN_PROTOCOL_VERSION,
                    crate::PROTOCOL_VERSION
                ),
            ));
        }
        if !self.session_key.ct_eq(expected_key) {
            return Err(HelloReject::new(
                RejectReason::BadKey,
                "invalid session key",
            ));
        }
        Ok(())
    }
}

/// Version-independent prefix of [`ClientHello`].
#[derive(Deserialize)]
struct HelloPrefix {
    magic: [u8; 4],
    proto_version: u32,
}

/// Read just the magic and protocol version from an encoded hello payload,
/// regardless of what the rest of the frame looks like. Lets a daemon reply
/// [`RejectReason::VersionMismatch`] to clients from other versions.
pub fn peek_hello_version(payload: &[u8]) -> Result<u32, ProtoError> {
    let (prefix, _rest): (HelloPrefix, _) = postcard::take_from_bytes(payload)?;
    if prefix.magic != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    Ok(prefix.proto_version)
}

/// Daemon's successful reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHello {
    /// Protocol version the daemon will speak on this connection.
    pub proto_version: u32,
    /// `civetd` version (semver).
    pub daemon_version: String,
    /// Random id chosen at daemon startup. If it differs from the one the
    /// client saw before, the daemon restarted: pane ids, screen state numbers
    /// and input sequence numbers from the old instance are void.
    pub instance_id: u64,
    /// True if the daemon already had state for this [`ClientId`] (the client
    /// can resume subscriptions with its known state numbers).
    pub resumed: bool,
    /// Daemon wall clock, ms since epoch (for latency/skew diagnostics).
    pub server_time_ms: u64,
}

/// Why a hello was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RejectReason {
    /// Session key did not match (daemon restarted with a new key: re-bootstrap).
    BadKey,
    /// Incompatible protocol version (upgrade the daemon or the client).
    VersionMismatch,
    /// Not a Civet hello.
    BadMagic,
    /// Daemon is shutting down / upgrading; retry after re-bootstrap.
    ShuttingDown,
}

/// Daemon's refusal; the daemon closes the connection after sending it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloReject {
    /// Machine-readable reason.
    pub reason: RejectReason,
    /// Human-readable detail.
    pub message: String,
}

impl HelloReject {
    /// Convenience constructor.
    pub fn new(reason: RejectReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// First frame the daemon sends on the control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelloReply {
    /// Authenticated; steady-state messages follow.
    Accepted(ServerHello),
    /// Refused; connection will be closed.
    Rejected(HelloReject),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{decode, encode};

    fn hello() -> ClientHello {
        ClientHello::new(SessionKey([9; 32]), ClientId([3; 16]), "civet-cli", "0.1.0")
    }

    #[test]
    fn roundtrip() {
        let h = hello();
        assert_eq!(decode::<ClientHello>(&encode(&h).unwrap()).unwrap(), h);

        for reply in [
            HelloReply::Accepted(ServerHello {
                proto_version: 1,
                daemon_version: "0.1.0".into(),
                instance_id: 77,
                resumed: true,
                server_time_ms: 123,
            }),
            HelloReply::Rejected(HelloReject::new(RejectReason::BadKey, "nope")),
        ] {
            assert_eq!(
                decode::<HelloReply>(&encode(&reply).unwrap()).unwrap(),
                reply
            );
        }
    }

    #[test]
    fn validate() {
        let h = hello();
        assert!(h.validate(&SessionKey([9; 32])).is_ok());
        assert_eq!(
            h.validate(&SessionKey([8; 32])).unwrap_err().reason,
            RejectReason::BadKey
        );
        let mut v = hello();
        v.proto_version = 999;
        assert_eq!(
            v.validate(&SessionKey([9; 32])).unwrap_err().reason,
            RejectReason::VersionMismatch
        );
        let mut m = hello();
        m.magic = *b"HTTP";
        assert_eq!(
            m.validate(&SessionKey([9; 32])).unwrap_err().reason,
            RejectReason::BadMagic
        );
    }

    #[test]
    fn peek_version_works_on_future_layouts() {
        // A hypothetical future hello with extra/different fields after the
        // frozen prefix.
        #[derive(Serialize)]
        struct FutureHello {
            magic: [u8; 4],
            proto_version: u32,
            something_new: Vec<String>,
        }
        let bytes = encode(&FutureHello {
            magic: MAGIC,
            proto_version: 42,
            something_new: vec!["x".into()],
        })
        .unwrap();
        assert_eq!(peek_hello_version(&bytes).unwrap(), 42);
        assert_eq!(peek_hello_version(&encode(&hello()).unwrap()).unwrap(), 1);

        let bad = encode(&(*b"GET ", 1u32)).unwrap();
        assert!(matches!(
            peek_hello_version(&bad),
            Err(ProtoError::BadMagic)
        ));
        assert!(peek_hello_version(&[]).is_err());
    }
}
