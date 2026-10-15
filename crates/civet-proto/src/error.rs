//! Error type for encoding, decoding and framing.

use thiserror::Error;

/// Errors produced by this crate.
#[derive(Debug, Error)]
pub enum ProtoError {
    /// Postcard (de)serialization failed: malformed or incompatible payload.
    #[error("postcard codec error: {0}")]
    Codec(#[from] postcard::Error),

    /// JSON (de)serialization failed (bootstrap line).
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A frame header announced a payload larger than the allowed maximum.
    #[error("frame of {len} bytes exceeds maximum of {max} bytes")]
    FrameTooLarge {
        /// Announced payload length.
        len: usize,
        /// Maximum accepted payload length.
        max: usize,
    },

    /// The stream ended in the middle of a frame.
    #[error("stream ended mid-frame ({0} bytes buffered)")]
    Truncated(usize),

    /// Underlying I/O error (async framing helpers).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A hex string (session key / fingerprint) was malformed.
    #[error("invalid hex value: {0}")]
    InvalidHex(String),

    /// The hello frame did not start with [`crate::MAGIC`].
    #[error("bad magic: not a Civet client")]
    BadMagic,

    /// No bootstrap line was found in the daemon's output.
    #[error("no civetd bootstrap line found in output")]
    NoBootstrapLine,

    /// The OS random number generator failed.
    #[error("random number generator failed: {0}")]
    Random(String),
}

impl From<ProtoError> for std::io::Error {
    fn from(e: ProtoError) -> Self {
        match e {
            ProtoError::Io(io) => io,
            other => std::io::Error::new(std::io::ErrorKind::InvalidData, other),
        }
    }
}
