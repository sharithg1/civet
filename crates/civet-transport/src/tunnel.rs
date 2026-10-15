//! Listening-port updates and TCP port forwarding, on their own QUIC
//! streams (see `civet_proto::ports`). QUIC only: on stream links the
//! client calls fail with `ErrorCode::Unsupported` and the daemon never
//! sees these streams.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use civet_proto::frame::{read_raw_frame, read_raw_frame_limited};
use civet_proto::{
    ErrorCode, ForwardReply, PortsUpdate, RpcError, StreamOpen, decode, encode_frame,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::conn::frame;
use crate::error::{TransportError, frame_error, from_conn_error, stream_error};

/// How long the client waits for the daemon's [`ForwardReply`] (the daemon
/// connects to a local port, so this is mostly one round trip).
pub const FORWARD_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest `ForwardReply` frame (an `RpcError` message at most).
const MAX_FORWARD_REPLY_LEN: usize = 4096;

/// QUIC send priority of forwarded connections: below the control and
/// screen streams (0), so a large download never delays keystrokes or
/// repaints.
const FORWARD_PRIORITY: i32 = -1;

/// The error a client gets for these streams on a stream link.
pub(crate) fn unsupported_on_stream_link() -> RpcError {
    RpcError::new(
        ErrorCode::Unsupported,
        "port forwarding needs the QUIC transport (this host is connected over the SSH tunnel)",
    )
}

// ---------------------------------------------------------------------------
// Daemon side
// ---------------------------------------------------------------------------

/// Daemon side of a `StreamOpen::Ports` stream.
pub struct PortsSink {
    pub(crate) send: quinn::SendStream,
    pub(crate) conn: quinn::Connection,
}

impl fmt::Debug for PortsSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PortsSink")
    }
}

impl PortsSink {
    /// Send one update.
    pub async fn send(&mut self, update: &PortsUpdate) -> Result<(), TransportError> {
        let bytes = frame(update)?;
        self.send
            .write_all(&bytes)
            .await
            .map_err(|e| stream_error(&self.conn, e))
    }

    /// Resolves once the client stopped reading (closed the stream) or the
    /// connection is gone.
    pub async fn stopped(&mut self) {
        let _ = self.send.stopped().await;
    }
}

/// Daemon side of a `StreamOpen::Forward` stream, not answered yet.
pub struct ForwardRequest {
    pub(crate) port: u16,
    pub(crate) send: quinn::SendStream,
    pub(crate) recv: quinn::RecvStream,
    pub(crate) conn: quinn::Connection,
}

impl fmt::Debug for ForwardRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForwardRequest")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl ForwardRequest {
    /// The port the client wants.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Answer `Ok`; the stream then carries raw bytes.
    pub async fn accept(mut self) -> Result<Tunnel, TransportError> {
        let _ = self.send.set_priority(FORWARD_PRIORITY);
        let reply: ForwardReply = Ok(());
        self.send
            .write_all(&encode_frame(&reply)?)
            .await
            .map_err(|e| stream_error(&self.conn, e))?;
        Ok(Tunnel {
            send: self.send,
            recv: self.recv,
        })
    }

    /// Answer with an error and finish the stream.
    pub async fn reject(mut self, error: RpcError) {
        let reply: ForwardReply = Err(error);
        if let Ok(bytes) = encode_frame(&reply) {
            let _ = self.send.write_all(&bytes).await;
        }
        let _ = self.send.finish();
        let _ = self.recv.stop(0u32.into());
    }
}

// ---------------------------------------------------------------------------
// Both sides
// ---------------------------------------------------------------------------

/// A forwarded TCP connection's byte stream (one QUIC bidi stream).
/// `shutdown` finishes the sending half (TCP half-close).
pub struct Tunnel {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tunnel")
    }
}

impl AsyncRead for Tunnel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tunnel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Client side of a `StreamOpen::Ports` stream. Dropping it closes the
/// stream (the daemon stops scanning for this client).
pub struct PortsWatch {
    recv: quinn::RecvStream,
    /// Kept open: finishing it early is harmless, but holding it makes the
    /// stream's lifetime obvious.
    _send: quinn::SendStream,
    conn: quinn::Connection,
}

impl fmt::Debug for PortsWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PortsWatch")
    }
}

impl PortsWatch {
    /// Next update; `Ok(None)` when the daemon ended the stream. A daemon
    /// that does not know `StreamOpen::Ports` resets the stream at once, so
    /// an error before the first update means "unsupported".
    pub async fn next(&mut self) -> Result<Option<PortsUpdate>, TransportError> {
        match read_raw_frame(&mut self.recv).await {
            Ok(Some(p)) => Ok(Some(decode::<PortsUpdate>(&p)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(frame_error(&self.conn, e)),
        }
    }
}

pub(crate) async fn watch_ports(conn: &quinn::Connection) -> Result<PortsWatch, TransportError> {
    let (mut send, recv) = conn.open_bi().await.map_err(|e| from_conn_error(&e))?;
    send.write_all(&frame(&StreamOpen::Ports)?)
        .await
        .map_err(|e| stream_error(conn, e))?;
    Ok(PortsWatch {
        recv,
        _send: send,
        conn: conn.clone(),
    })
}

/// Open a forward stream and wait for the daemon's reply. `Ok(Err(_))`: the
/// daemon refused (nothing listens there, or it does not support
/// forwarding: it reset the stream).
pub(crate) async fn open_forward(
    conn: &quinn::Connection,
    port: u16,
) -> Result<Result<Tunnel, RpcError>, TransportError> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| from_conn_error(&e))?;
    let _ = send.set_priority(FORWARD_PRIORITY);
    send.write_all(&frame(&StreamOpen::Forward { port })?)
        .await
        .map_err(|e| stream_error(conn, e))?;
    let reply = tokio::time::timeout(
        FORWARD_REPLY_TIMEOUT,
        read_raw_frame_limited(&mut recv, MAX_FORWARD_REPLY_LEN),
    )
    .await
    .map_err(|_| TransportError::Timeout("forward reply"))?;
    let reply = match reply {
        Ok(Some(p)) => decode::<ForwardReply>(&p)?,
        Ok(None) => return Ok(Err(old_daemon())),
        Err(e) => {
            if conn.close_reason().is_some() {
                return Err(frame_error(conn, e));
            }
            // Stream reset with the connection alive: a daemon that does
            // not know `StreamOpen::Forward`.
            return Ok(Err(old_daemon()));
        }
    };
    Ok(reply.map(|()| Tunnel { send, recv }))
}

fn old_daemon() -> RpcError {
    RpcError::new(
        ErrorCode::Unsupported,
        "this daemon does not support port forwarding (restart it to upgrade)",
    )
}
