//! Wire protocol for Civet.
//!
//! This crate is the single source of truth for every byte that crosses the
//! network between the desktop client (or `civet-cli`) and the `civetd`
//! daemon. It has no networking code of its own: `civet-transport` moves the
//! frames, `civetd` / `civet-session` produce and consume the messages.
//!
//! # Layers
//!
//! * [`bootstrap`]: the single JSON line `civetd` prints on stdout when it is
//!   started over SSH ([`BootstrapInfo`]).
//! * [`hello`]: the first frame on the control stream ([`ClientHello`]) and the
//!   daemon's reply ([`HelloReply`]). Authenticates with the [`SessionKey`].
//! * [`messages`]: the steady-state envelopes [`ClientMessage`] and
//!   [`ServerMessage`], plus [`StreamOpen`], the header that identifies every
//!   QUIC stream after the control stream.
//! * [`control`]: control RPC ([`Request`] / [`Response`]) and pushed
//!   [`Event`]s.
//! * [`term`]: terminal traffic: [`InputMessage`] (client keystrokes with
//!   sequence numbers), [`ScreenUpdate`] (server screen diffs as VT bytes),
//!   [`ScreenAck`], and scrollback paging.
//! * [`frame`]: length-prefixed framing (`u32` big-endian length + postcard
//!   payload), sync and async (tokio) helpers.
//!
//! # Encoding and versioning
//!
//! Every frame payload is [postcard](https://docs.rs/postcard). Postcard is not
//! self-describing: enum variants are encoded by index and struct fields by
//! position. Therefore, within one [`PROTOCOL_VERSION`]:
//!
//! * enum variants may only be **appended** (never reordered or removed);
//! * struct fields must never be added, removed or reordered — doing so
//!   requires bumping [`PROTOCOL_VERSION`].
//!
//! The hello frame starts with [`MAGIC`] and the protocol version so a peer of
//! any version can read those two fields ([`hello::peek_hello_version`]) and
//! reject a mismatch cleanly.
//!
//! The types also implement JSON-friendly serde (camelCase field names, hex
//! strings for keys) so the desktop app can hand them to its web UI unchanged.
//! Attributes that postcard cannot handle (`#[serde(tag = ...)]`,
//! `#[serde(untagged)]`, `#[serde(flatten)]`, `skip_serializing_if`) must not
//! be used in this crate.

pub mod agent;
pub mod bootstrap;
pub mod control;
pub mod error;
pub mod frame;
pub mod hello;
pub mod ids;
pub mod keys;
pub mod messages;
pub mod term;

pub use agent::{AgentStatus, AgentStatusSource};
pub use bootstrap::{BOOTSTRAP_FORMAT, BootstrapInfo};
pub use control::{
    AgentStatusEvent, CommandSpec, ControlRequest, ControlResponse, DaemonInfo, ErrorCode, Event,
    KillSignal, PaneInfo, RepoInfo, Request, Response, RpcError, SpawnPaneRequest, SubscribeInfo,
    WorktreeInfo,
};
pub use error::ProtoError;
pub use frame::{FrameDecoder, MAX_FRAME_LEN, decode, encode, encode_frame};
pub use hello::{ClientHello, HelloReject, HelloReply, RejectReason, ServerHello};
pub use ids::{ClientId, PaneId, RepoId, RequestId, WorktreeId};
pub use keys::{CertFingerprint, SessionKey};
pub use messages::{ClientMessage, ServerMessage, StreamOpen};
pub use term::{
    InputMessage, ScreenAck, ScreenUpdate, ScrollbackLine, ScrollbackPage, ScrollbackQuery,
    ScrollbackRequest, TermSize, UpdateKind,
};

/// Wire protocol version. Bump on any incompatible change (see crate docs).
pub const PROTOCOL_VERSION: u32 = 1;

/// Oldest protocol version this build can talk to. Equal to
/// [`PROTOCOL_VERSION`] until we deliberately support a range.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// First four bytes of every [`ClientHello`]; lets the daemon reject
/// non-Civet traffic on its UDP port immediately.
pub const MAGIC: [u8; 4] = *b"CIVT";

/// ALPN protocol id used for the QUIC/TLS handshake.
pub const ALPN: &[u8] = b"civet/1";

/// Milliseconds since the Unix epoch, used for timestamps in messages.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Returns true if a peer speaking `version` is compatible with this build.
pub fn is_compatible(version: u32) -> bool {
    (MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}
