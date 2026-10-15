//! Transport for Civet.
//!
//! Moves `civet-proto` frames between client and daemon over QUIC (quinn,
//! self-signed certificate pinned by SHA-256 fingerprint, session-key hello,
//! connection migration, one stream per purpose), or over any ordered byte
//! stream (SSH stdio tunnel, unix socket, tests) behind the same API, so
//! higher layers do not care which.
//!
//! * Daemon: [`QuicServer`] / [`accept_stream`] → [`ServerConnection`] →
//!   ([`ServerSender`], [`ServerReceiver`]).
//! * Client, one connection: [`QuicClient::connect`] / [`connect_stream`] →
//!   [`Connected`] ([`ClientConnection`], [`ClientReceiver`]).
//! * Client, resumable: [`ClientSession`] (reconnect, resume token, input
//!   retransmission, re-subscription, stall detection, racing re-dial).
//!
//! See `docs/INTERFACES.md` section 2 for the contract.

pub mod cert;
mod conn;
pub mod error;
pub mod failover;
mod quic;
pub mod session;
mod stream;

pub use cert::{SERVER_NAME, ServerIdentity, fingerprint_of};
pub use conn::{
    ClientConnection, ClientReceiver, Connected, ScreenStream, ScrollbackResponder,
    ServerConnection, ServerIncoming, ServerReceiver, ServerSender, TransportKind,
};
pub use error::{TransportError, close_code};
pub use failover::{FailoverConfig, FailoverDialer};
pub use quic::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_PORT_RANGE, HELLO_TIMEOUT, IDLE_TIMEOUT, INITIAL_RTT,
    IncomingConnection, KEEP_ALIVE, QuicClient, QuicClientConfig, QuicServer, QuicServerConfig,
};
pub use session::{
    ClientSession, ClientSessionConfig, Dialer, LinkState, QuicDialer, REPLACED_REASON,
    REPLACED_RETRY_DELAY, REPLACED_WINDOW, RequestError, SessionEvent, StreamDialer,
};
pub use stream::{STREAM_CONNECT_TIMEOUT, accept_stream, connect_stream};

use civet_proto::{ClientHello, HelloReject, RejectReason, SessionKey};

/// Decode and validate a hello payload (magic, version, key in constant
/// time). Returns the rejection to send on failure.
pub(crate) fn parse_hello(payload: &[u8], key: &SessionKey) -> Result<ClientHello, HelloReject> {
    match civet_proto::hello::peek_hello_version(payload) {
        Ok(v) if !civet_proto::is_compatible(v) => {
            return Err(HelloReject::new(
                RejectReason::VersionMismatch,
                format!(
                    "client protocol v{v} not supported (daemon supports v{}..=v{})",
                    civet_proto::MIN_PROTOCOL_VERSION,
                    civet_proto::PROTOCOL_VERSION
                ),
            ));
        }
        Ok(_) => {}
        Err(_) => {
            return Err(HelloReject::new(
                RejectReason::BadMagic,
                "not a civet hello",
            ));
        }
    }
    let hello: ClientHello = civet_proto::decode(payload)
        .map_err(|_| HelloReject::new(RejectReason::BadMagic, "malformed hello"))?;
    hello.validate(key)?;
    Ok(hello)
}

#[cfg(test)]
mod tests {
    use super::*;
    use civet_proto::{ClientId, PaneId, ScreenUpdate, ServerMessage, TermSize, UpdateKind};

    #[test]
    fn screen_frame_matches_owned_encoding() {
        let u = ScreenUpdate {
            pane: PaneId(3),
            state: 9,
            base_state: 8,
            kind: UpdateKind::Diff,
            acked_input_seq: 4,
            size: TermSize::new(80, 24),
            history_lines: 10,
            history_start: 10,
            payload: b"\x1b[Hhi".to_vec(),
        };
        let owned = civet_proto::encode_frame(&ServerMessage::Screen(u.clone())).unwrap();
        assert_eq!(&conn::encode_screen_frame(&u).unwrap()[..], &owned[..]);
    }

    #[test]
    fn hello_parsing() {
        let key = SessionKey([1; 32]);
        let hello = ClientHello::new(key.clone(), ClientId([2; 16]), "t", "0");
        let ok = civet_proto::encode(&hello).unwrap();
        assert_eq!(parse_hello(&ok, &key).unwrap(), hello);
        assert_eq!(
            parse_hello(&ok, &SessionKey([0; 32])).unwrap_err().reason,
            RejectReason::BadKey
        );
        let mut v = hello.clone();
        v.proto_version = 77;
        let v = civet_proto::encode(&v).unwrap();
        assert_eq!(
            parse_hello(&v, &key).unwrap_err().reason,
            RejectReason::VersionMismatch
        );
        assert_eq!(
            parse_hello(b"GET / HTTP/1.1", &key).unwrap_err().reason,
            RejectReason::BadMagic
        );
    }
}
