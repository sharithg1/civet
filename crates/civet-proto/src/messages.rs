//! Stream headers and steady-state message envelopes.
//!
//! # Stream layout on a QUIC connection
//!
//! Every stream starts with one [`StreamOpen`] frame identifying it.
//!
//! | Stream | Opened by | Frames after `StreamOpen` |
//! |---|---|---|
//! | `Control` (bidi, exactly one, first) | client | C->S: [`crate::ClientHello`], then [`ClientMessage`]*; S->C: [`crate::HelloReply`], then [`ServerMessage`]* |
//! | `Screen { pane }` (uni) | daemon | [`ServerMessage::Screen`]* for that pane, in state order |
//! | `Scrollback` (bidi, one per request) | client | C->S: one [`crate::ScrollbackRequest`]; S->C: one `Result<ScrollbackPage, RpcError>`, then FIN |
//! | `Ports` (bidi, at most one) | client | S->C: [`crate::PortsUpdate`]* (see [`crate::ports`]) |
//! | `Forward { port }` (bidi, one per TCP connection) | client | S->C: one [`crate::ForwardReply`], then raw TCP bytes both ways |
//!
//! When everything has to share one ordered byte stream (the SSH-tunnel
//! fallback, or tests), screen updates may instead be sent on the control
//! stream as [`ServerMessage::Screen`] and scrollback via
//! [`crate::Request::FetchScrollback`]; receivers must accept both.

use serde::{Deserialize, Serialize};

use crate::control::{ControlRequest, ControlResponse, Event};
use crate::ids::PaneId;
use crate::term::{InputMessage, ScreenAck, ScreenUpdate};

/// First frame on every QUIC stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum StreamOpen {
    /// The control stream.
    Control,
    /// Screen updates for one pane (daemon-opened uni stream).
    Screen {
        /// Pane.
        pane: PaneId,
    },
    /// One scrollback request/response exchange.
    Scrollback,
    /// Listening-port updates (see [`crate::ports`]).
    Ports,
    /// One forwarded TCP connection to `port` on the daemon's loopback
    /// interface (see [`crate::ports`]).
    Forward {
        /// TCP port.
        port: u16,
    },
}

/// Messages client -> daemon on the control stream (after the hello).
/// Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientMessage {
    /// Control RPC.
    Request(ControlRequest),
    /// Keystrokes for a pane.
    Input(InputMessage),
    /// Screen state acknowledgement.
    Ack(ScreenAck),
    /// Client is disconnecting on purpose (don't keep resume state hot).
    Goodbye,
}

/// Messages daemon -> client on the control stream (after the hello), and on
/// screen streams (only the `Screen` variant). Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    /// Control RPC response.
    Response(ControlResponse),
    /// Pushed event.
    Event(Event),
    /// Screen update.
    Screen(ScreenUpdate),
    /// Input was applied but produced no screen change worth an update; lets
    /// the client retire unacked input / predictions.
    InputAck {
        /// Pane.
        pane: PaneId,
        /// Highest applied input seq.
        seq: u64,
    },
    /// Daemon is shutting down or upgrading; client should re-bootstrap.
    Goodbye {
        /// Human-readable reason.
        reason: String,
    },
}
