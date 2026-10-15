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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
