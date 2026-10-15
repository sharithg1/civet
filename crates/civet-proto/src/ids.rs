//! Identifier newtypes.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ProtoError;
use crate::keys::{hex_array, parse_hex, random_bytes};

/// Correlates a [`crate::ControlRequest`] with its [`crate::ControlResponse`].
/// Chosen by the client, unique per connection (a simple counter is fine).
pub type RequestId = u64;

/// A terminal pane (one PTY + one server-side terminal emulator).
/// Allocated by the daemon; unique for the daemon's lifetime and never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub u64);

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pane-{}", self.0)
    }
}

/// A repository registered with the daemon. Opaque to the client; the daemon
/// derives it from the repository's canonical path, so it is stable across
/// daemon restarts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(pub String);

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RepoId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// A git worktree (including a repository's main worktree). Opaque to the
/// client; the daemon uses the worktree's canonical absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeId(pub String);

impl fmt::Display for WorktreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for WorktreeId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// Stable identity of a client installation (one per desktop app / CLI
/// profile). Lets the daemon keep per-client state (last acked screen state,
/// last input sequence number) across QUIC reconnects.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub [u8; 16]);

impl ClientId {
    /// Generate a random client id.
    pub fn generate() -> Result<Self, ProtoError> {
        let mut b = [0u8; 16];
        random_bytes(&mut b)?;
        Ok(Self(b))
    }

    /// Lowercase hex encoding (32 chars).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse from a 32-char hex string.
    pub fn from_hex(s: &str) -> Result<Self, ProtoError> {
        parse_hex(s).map(Self)
    }
}

impl fmt::Debug for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientId({})", self.to_hex())
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for ClientId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        hex_array::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for ClientId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        hex_array::deserialize(d).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_transparent_in_json() {
        assert_eq!(serde_json::to_string(&PaneId(42)).unwrap(), "42");
        assert_eq!(serde_json::to_string(&RepoId::from("r")).unwrap(), "\"r\"");
        assert_eq!(
            serde_json::to_string(&WorktreeId::from("/w")).unwrap(),
            "\"/w\""
        );
    }

    #[test]
    fn client_id_roundtrip() {
        let id = ClientId::generate().unwrap();
        assert_eq!(ClientId::from_hex(&id.to_hex()).unwrap(), id);
        let bin = postcard::to_stdvec(&id).unwrap();
        assert_eq!(bin.len(), 16);
        assert_eq!(postcard::from_bytes::<ClientId>(&bin).unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ClientId>(&json).unwrap(), id);
    }
}
