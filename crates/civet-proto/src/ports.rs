//! Port detection and TCP port forwarding.
//!
//! Both run on their own client-opened QUIC streams, never on the control
//! stream, so a daemon that predates them just drops the stream (it reads an
//! unknown [`crate::StreamOpen`] variant and gives up on that stream only):
//! the connection is unaffected and no version check is needed.
//!
//! * `StreamOpen::Ports` (bidi, at most one per connection): the client
//!   sends nothing after the header; the daemon sends one [`PortsUpdate`]
//!   at once and another whenever the set of listening TCP ports changes.
//!   The client closes the stream to stop.
//! * `StreamOpen::Forward { port }` (bidi, one per forwarded TCP
//!   connection): the daemon connects to `port` on its loopback interface
//!   and answers with one [`ForwardReply`] frame. After an `Ok` the stream
//!   carries the raw TCP bytes both ways (no framing); FIN on either side is
//!   a TCP half-close.
//!
//! Neither works on the stream (stdio-tunnel) link, which has a single
//! ordered byte stream.

use serde::{Deserialize, Serialize};

use crate::control::RpcError;
use crate::ids::{PaneId, WorktreeId};

/// A TCP port something listens on, on the daemon's host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    /// TCP port.
    pub port: u16,
    /// Addresses it is bound to (`127.0.0.1`, `::1`, `0.0.0.0`, `::`...).
    pub addrs: Vec<String>,
    /// Owning process, if it could be determined.
    pub pid: Option<u32>,
    /// Process name (`node`, `python3`...).
    pub process: Option<String>,
    /// Command line, truncated to [`MAX_PORT_COMMAND_CHARS`].
    pub command: Option<String>,
    /// Pane whose process tree owns the socket.
    pub pane: Option<PaneId>,
    /// Worktree: the pane's, else the one containing the process's cwd.
    pub worktree: Option<WorktreeId>,
}

/// Every port the daemon's user listens on, sorted by port. Sent on a
/// `StreamOpen::Ports` stream.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortsUpdate {
    /// The ports.
    pub ports: Vec<ListeningPort>,
}

/// The daemon's answer on a `StreamOpen::Forward` stream: `Ok` once it is
/// connected to the port (raw bytes follow), else why not (`NotFound`:
/// nothing listens there).
pub type ForwardReply = Result<(), RpcError>;

/// Longest [`ListeningPort::command`], in chars.
pub const MAX_PORT_COMMAND_CHARS: usize = 200;
