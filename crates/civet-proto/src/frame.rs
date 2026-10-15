//! Length-prefixed framing.
//!
//! A frame is a 4-byte big-endian payload length followed by the payload.
//! Payloads are postcard-encoded messages ([`encode`] / [`decode`]). The
//! `*_raw_*` helpers move already-encoded payloads (e.g. a hello peeked with
//! [`crate::hello::peek_hello_version`] before full decoding).
//!
//! The same framing is used on every QUIC stream and on the SSH/stdio
//! fallback, where the whole session is one ordered byte stream.
//!
//! ```
//! use civet_proto::{FrameDecoder, encode_frame, ScreenAck, PaneId};
//!
//! let ack = ScreenAck { pane: PaneId(1), state: 7 };
//! let bytes = encode_frame(&ack).unwrap();
//!
//! let mut dec = FrameDecoder::new();
//! dec.push(&bytes[..3]);
//! assert!(dec.next::<ScreenAck>().unwrap().is_none()); // incomplete
//! dec.push(&bytes[3..]);
//! assert_eq!(dec.next::<ScreenAck>().unwrap(), Some(ack));
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ProtoError;

/// Size of the length prefix in bytes.
pub const HEADER_LEN: usize = 4;

/// Largest payload accepted (16 MiB). Screen repaints and scrollback pages are
/// far below this; anything larger is treated as a protocol error.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Postcard-encode a message (no length prefix).
pub fn encode<T: Serialize + ?Sized>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    Ok(postcard::to_stdvec(msg)?)
}

/// Postcard-decode a message (no length prefix). Trailing bytes are an error.
pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtoError> {
    let (msg, rest) = postcard::take_from_bytes(payload)?;
    if !rest.is_empty() {
        return Err(ProtoError::Codec(postcard::Error::DeserializeBadEncoding));
    }
    Ok(msg)
}

/// Prefix an already-encoded payload with its length.
pub fn frame_raw(payload: &[u8]) -> Result<Vec<u8>, ProtoError> {
    check_len(payload.len())?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Encode a message as a complete frame (length prefix + postcard payload).
pub fn encode_frame<T: Serialize + ?Sized>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    // Serialize directly after a placeholder header to avoid a second copy.
    let mut out = postcard::to_extend(msg, vec![0u8; HEADER_LEN])?;
    let len = out.len() - HEADER_LEN;
    check_len(len)?;
    out[..HEADER_LEN].copy_from_slice(&(len as u32).to_be_bytes());
    Ok(out)
}

/// Append a frame for `msg` to `buf`.
pub fn encode_frame_into<T: Serialize + ?Sized>(
    msg: &T,
    buf: &mut BytesMut,
) -> Result<(), ProtoError> {
    let payload = encode(msg)?;
    check_len(payload.len())?;
    buf.reserve(HEADER_LEN + payload.len());
    buf.put_u32(payload.len() as u32);
    buf.put_slice(&payload);
    Ok(())
}

fn check_len(len: usize) -> Result<(), ProtoError> {
    if len > MAX_FRAME_LEN {
        Err(ProtoError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        })
    } else {
        Ok(())
    }
}

/// Incremental decoder for a byte stream of frames. Feed it whatever chunks
/// arrive with [`push`](Self::push), then drain complete frames with
/// [`next`](Self::next) / [`next_raw`](Self::next_raw). Never blocks, so it
/// is safe to use inside `select!` loops.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: BytesMut,
    max_len: usize,
}

impl FrameDecoder {
    /// Decoder with the default [`MAX_FRAME_LEN`].
    pub fn new() -> Self {
        Self::with_max_len(MAX_FRAME_LEN)
    }

    /// Decoder with a custom payload size limit.
    pub fn with_max_len(max_len: usize) -> Self {
        Self {
            buf: BytesMut::new(),
            max_len,
        }
    }

    /// Append received bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Number of buffered, not yet consumed bytes.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Pop the next complete frame's payload, if one is buffered.
    pub fn next_raw(&mut self) -> Result<Option<Bytes>, ProtoError> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buf[..HEADER_LEN].try_into().expect("4 bytes")) as usize;
        if len > self.max_len {
            return Err(ProtoError::FrameTooLarge {
                len,
                max: self.max_len,
            });
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        self.buf.advance(HEADER_LEN);
        Ok(Some(self.buf.split_to(len).freeze()))
    }

    /// Pop and decode the next complete frame, if one is buffered.
    #[allow(clippy::should_implement_trait)] // fallible + generic; not an Iterator
    pub fn next<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ProtoError> {
        match self.next_raw()? {
            Some(payload) => decode(&payload).map(Some),
            None => Ok(None),
        }
    }

    /// Call at end of stream: errors if a partial frame is still buffered.
    pub fn finish(&self) -> Result<(), ProtoError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::Truncated(self.buf.len()))
        }
    }
}

#[cfg(feature = "tokio")]
mod async_io {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    /// Write one already-encoded payload as a frame and flush.
    pub async fn write_raw_frame<W>(w: &mut W, payload: &[u8]) -> Result<(), ProtoError>
    where
        W: AsyncWrite + Unpin + ?Sized,
    {
        check_len(payload.len())?;
        w.write_all(&(payload.len() as u32).to_be_bytes()).await?;
        w.write_all(payload).await?;
        w.flush().await?;
        Ok(())
    }

    /// Encode `msg` and write it as one frame, then flush.
    pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), ProtoError>
    where
        W: AsyncWrite + Unpin + ?Sized,
        T: Serialize + ?Sized,
    {
        let frame = encode_frame(msg)?;
        w.write_all(&frame).await?;
        w.flush().await?;
        Ok(())
    }

    /// Read one frame's payload. Returns `Ok(None)` on a clean end of stream
    /// at a frame boundary and [`ProtoError::Truncated`] if the stream ends
    /// mid-frame.
    ///
    /// **Not cancel-safe**: if the future is dropped mid-frame, bytes are lost.
    /// Run it in a dedicated reader task, or use [`FrameDecoder`] in
    /// `select!` loops.
    pub async fn read_raw_frame<R>(r: &mut R) -> Result<Option<Vec<u8>>, ProtoError>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        let mut header = [0u8; HEADER_LEN];
        let mut got = 0;
        while got < HEADER_LEN {
            let n = r.read(&mut header[got..]).await?;
            if n == 0 {
                return if got == 0 {
                    Ok(None)
                } else {
                    Err(ProtoError::Truncated(got))
                };
            }
            got += n;
        }
        let len = u32::from_be_bytes(header) as usize;
        check_len(len)?;
        let mut payload = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            let n = r.read(&mut payload[filled..]).await?;
            if n == 0 {
                return Err(ProtoError::Truncated(HEADER_LEN + filled));
            }
            filled += n;
        }
        Ok(Some(payload))
    }

    /// Read and decode one frame. Same EOF semantics and cancel-safety caveat
    /// as [`read_raw_frame`].
    pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>, ProtoError>
    where
        R: AsyncRead + Unpin + ?Sized,
        T: DeserializeOwned,
    {
        match read_raw_frame(r).await? {
            Some(p) => decode(&p).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(feature = "tokio")]
pub use async_io::{read_frame, read_raw_frame, write_frame, write_raw_frame};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneId, ScreenAck};

    #[test]
    fn encode_frame_matches_manual() {
        let ack = ScreenAck {
            pane: PaneId(300),
            state: 5,
        };
        let payload = encode(&ack).unwrap();
        assert_eq!(encode_frame(&ack).unwrap(), frame_raw(&payload).unwrap());
        let mut b = BytesMut::new();
        encode_frame_into(&ack, &mut b).unwrap();
        assert_eq!(&b[..], &encode_frame(&ack).unwrap()[..]);
    }

    #[test]
    fn decoder_handles_byte_by_byte_and_multiple_frames() {
        let msgs: Vec<ScreenAck> = (0..5)
            .map(|i| ScreenAck {
                pane: PaneId(i),
                state: i * 1000,
            })
            .collect();
        let mut stream = Vec::new();
        for m in &msgs {
            stream.extend(encode_frame(m).unwrap());
        }
        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        for b in &stream {
            dec.push(std::slice::from_ref(b));
            while let Some(m) = dec.next::<ScreenAck>().unwrap() {
                out.push(m);
            }
        }
        assert_eq!(out, msgs);
        dec.finish().unwrap();
    }

    #[test]
    fn decoder_rejects_oversized_and_reports_truncation() {
        let mut dec = FrameDecoder::with_max_len(8);
        dec.push(&100u32.to_be_bytes());
        assert!(matches!(
            dec.next_raw(),
            Err(ProtoError::FrameTooLarge { len: 100, max: 8 })
        ));

        let mut dec = FrameDecoder::new();
        dec.push(&[0, 0, 0, 5, 1, 2]);
        assert!(dec.next_raw().unwrap().is_none());
        assert!(matches!(dec.finish(), Err(ProtoError::Truncated(6))));
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let mut p = encode(&7u32).unwrap();
        p.push(0);
        assert!(decode::<u32>(&p).is_err());
    }

    #[test]
    fn empty_payload_frame() {
        let f = frame_raw(&[]).unwrap();
        assert_eq!(f, vec![0, 0, 0, 0]);
        let mut dec = FrameDecoder::new();
        dec.push(&f);
        assert_eq!(dec.next_raw().unwrap().unwrap().len(), 0);
    }

    #[test]
    fn oversized_encode_rejected() {
        let big = vec![0u8; MAX_FRAME_LEN + 1];
        assert!(matches!(
            frame_raw(&big),
            Err(ProtoError::FrameTooLarge { .. })
        ));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_roundtrip_and_eof() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let msgs: Vec<ScreenAck> = (0..50)
            .map(|i| ScreenAck {
                pane: PaneId(i),
                state: u64::MAX - i,
            })
            .collect();
        let sent = msgs.clone();
        let writer = tokio::spawn(async move {
            for m in &sent {
                write_frame(&mut a, m).await.unwrap();
            }
            write_raw_frame(&mut a, b"raw").await.unwrap();
            // drop closes the stream
        });
        let mut got = Vec::new();
        for _ in 0..msgs.len() {
            got.push(read_frame::<_, ScreenAck>(&mut b).await.unwrap().unwrap());
        }
        assert_eq!(got, msgs);
        assert_eq!(read_raw_frame(&mut b).await.unwrap().unwrap(), b"raw");
        writer.await.unwrap();
        assert!(read_raw_frame(&mut b).await.unwrap().is_none());
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_truncated() {
        use tokio::io::AsyncWriteExt;
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[0, 0, 0, 10, 1, 2, 3]).await.unwrap();
        drop(a);
        assert!(matches!(
            read_raw_frame(&mut b).await,
            Err(ProtoError::Truncated(7))
        ));
    }
}
