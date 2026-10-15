//! The same framed protocol over one ordered byte stream (SSH stdio tunnel,
//! unix socket, in-memory duplex in tests).
//!
//! Everything shares the control stream: screen updates arrive as
//! `ServerMessage::Screen` and scrollback uses `Request::FetchScrollback` /
//! `Response::Scrollback` (routed back to `ClientConnection::fetch_scrollback`).

use std::sync::Arc;
use std::time::Duration;

use civet_proto::frame::read_raw_frame;
use civet_proto::{
    ClientHello, ClientMessage, HelloReject, HelloReply, ServerMessage, SessionKey, StreamOpen,
    decode,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::conn::{
    BoxedRead, BoxedWrite, ClientConnection, ClientReceiver, ClientShared, Closer, Connected,
    PendingServerLink, RxItem, ScrollRouter, ServerConnection, ServerIncoming, Tasks,
    TransportKind, WriteCmd, classify_client_message, frame, spawn_writer,
};
use crate::error::TransportError;
use crate::quic::HELLO_TIMEOUT;

/// Handshake timeout for `connect_stream` (covers e.g. a slow `ssh` start).
pub const STREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Same as `IncomingConnection::handshake` for an already-connected byte
/// stream (unix socket from `civetd stdio-tunnel`, or a test duplex).
pub async fn accept_stream<S>(
    io: S,
    session_key: &SessionKey,
) -> Result<ServerConnection, TransportError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read, write) = tokio::io::split(io);
    let mut read: BoxedRead = Box::new(read);
    let write: BoxedWrite = Box::new(write);
    let payload = tokio::time::timeout(HELLO_TIMEOUT, async {
        let open = read_raw_frame(&mut read)
            .await?
            .ok_or_else(|| TransportError::ProtocolViolation("stream ended before hello".into()))?;
        if decode::<StreamOpen>(&open).ok() != Some(StreamOpen::Control) {
            return Err(TransportError::ProtocolViolation(
                "stream link must start with StreamOpen::Control".into(),
            ));
        }
        read_raw_frame(&mut read)
            .await?
            .ok_or_else(|| TransportError::ProtocolViolation("stream ended before hello".into()))
    })
    .await
    .map_err(|_| TransportError::Timeout("waiting for client hello"))??;
    match crate::parse_hello(&payload, session_key) {
        Ok(hello) => Ok(ServerConnection {
            hello,
            link: PendingServerLink::Stream { read, write },
        }),
        Err(reject) => {
            tracing::info!(reason = ?reject.reason, "rejecting hello on stream link");
            reject_stream(write, &reject).await;
            Err(TransportError::Rejected(reject))
        }
    }
}

/// Write the rejection and close the stream.
pub(crate) async fn reject_stream(mut write: BoxedWrite, reject: &HelloReject) {
    if let Ok(bytes) = frame(&HelloReply::Rejected(reject.clone())) {
        let _ = write.write_all(&bytes).await;
        let _ = write.flush().await;
    }
    let _ = write.shutdown().await;
}

/// Daemon-side reader for a stream link.
pub(crate) fn spawn_server_reader(
    mut read: BoxedRead,
    tx: mpsc::Sender<RxItem<ServerIncoming>>,
    writer: mpsc::UnboundedSender<WriteCmd>,
    closer: Arc<Closer>,
) -> AbortHandle {
    tokio::spawn(async move {
        loop {
            match read_raw_frame(&mut read).await {
                Ok(Some(payload)) => match decode::<ClientMessage>(&payload) {
                    Ok(ClientMessage::Goodbye) => {
                        let _ = tx
                            .send(RxItem::Item(ServerIncoming::Message(
                                ClientMessage::Goodbye,
                            )))
                            .await;
                        let _ = tx.send(RxItem::End).await;
                        closer.close("client said goodbye");
                        return;
                    }
                    Ok(msg) => {
                        let item = classify_client_message(msg, &writer);
                        if tx.send(RxItem::Item(item)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        closer.close("protocol violation");
                        let _ = tx
                            .send(RxItem::Failed(TransportError::ProtocolViolation(format!(
                                "bad control frame: {e}"
                            ))))
                            .await;
                        return;
                    }
                },
                Ok(None) => {
                    closer.close("peer closed the stream");
                    let _ = tx.send(RxItem::End).await;
                    return;
                }
                Err(e) => {
                    closer.close(format!("read failed: {e}"));
                    let _ = tx.send(RxItem::Failed(TransportError::Proto(e))).await;
                    return;
                }
            }
        }
    })
    .abort_handle()
}

/// Same as `QuicClient::connect` over an arbitrary byte stream (SSH stdio
/// tunnel, unix socket, test duplex). Times out after
/// [`STREAM_CONNECT_TIMEOUT`].
pub async fn connect_stream<S>(
    io: S,
    hello: ClientHello,
    pipelined: Vec<ClientMessage>,
) -> Result<Connected, TransportError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read, write) = tokio::io::split(io);
    let mut read: BoxedRead = Box::new(read);
    let mut write: BoxedWrite = Box::new(write);
    let reply = tokio::time::timeout(STREAM_CONNECT_TIMEOUT, async {
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame(&StreamOpen::Control)?);
        buf.extend_from_slice(&frame(&hello)?);
        for m in &pipelined {
            buf.extend_from_slice(&frame(m)?);
        }
        write.write_all(&buf).await?;
        write.flush().await?;
        let reply = read_raw_frame(&mut read)
            .await?
            .ok_or_else(|| TransportError::Closed("stream ended before the hello reply".into()))?;
        Ok::<_, TransportError>(decode::<HelloReply>(&reply)?)
    })
    .await
    .map_err(|_| TransportError::Timeout("stream connect"))??;
    let server_hello = match reply {
        HelloReply::Accepted(h) => h,
        HelloReply::Rejected(r) => return Err(TransportError::Rejected(r)),
    };

    let closer = Closer::new();
    let tasks = Arc::new(Tasks::default());
    let scroll = Arc::new(ScrollRouter::default());
    let (wtx, wrx) = mpsc::unbounded_channel();
    let (itx, irx) = mpsc::unbounded_channel();
    tasks.push(spawn_writer(write, wrx, closer.clone()));
    {
        let closer = closer.clone();
        let scroll = scroll.clone();
        tasks.push(
            tokio::spawn(async move {
                let item = loop {
                    match read_raw_frame(&mut read).await {
                        Ok(Some(payload)) => match decode::<ServerMessage>(&payload) {
                            Ok(ServerMessage::Response(r)) => {
                                if let Some(r) = scroll.route(r)
                                    && itx.send(RxItem::Item(ServerMessage::Response(r))).is_err()
                                {
                                    break None;
                                }
                            }
                            Ok(msg) => {
                                if itx.send(RxItem::Item(msg)).is_err() {
                                    break None;
                                }
                            }
                            Err(e) => {
                                break Some(RxItem::Failed(TransportError::ProtocolViolation(
                                    format!("bad control frame: {e}"),
                                )));
                            }
                        },
                        Ok(None) => break Some(RxItem::End),
                        Err(e) => break Some(RxItem::Failed(TransportError::Proto(e))),
                    }
                };
                closer.close("stream link ended");
                scroll.clear();
                if let Some(item) = item {
                    let _ = itx.send(item);
                }
            })
            .abort_handle(),
        );
    }
    let shared = Arc::new(ClientShared {
        kind: TransportKind::Stream,
        writer: wtx,
        closer,
        quic: None,
        scroll,
        tasks,
    });
    Ok(Connected {
        conn: ClientConnection {
            inner: shared.clone(),
        },
        incoming: ClientReceiver {
            rx: irx,
            done: false,
            _shared: shared,
        },
        server_hello,
    })
}
