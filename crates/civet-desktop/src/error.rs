//! Command errors. Every command rejects with `{ code, message }` where
//! `code` is a proto `ErrorCode` name (`"notFound"`, …) or one of
//! `"transport"`, `"bootstrap"`, `"noSuchHost"` (docs/INTERFACES.md §6.1).

use civet_proto::{ErrorCode, RpcError};
use civet_transport::{RequestError, TransportError};
use serde::Serialize;

/// Serialized as `{ code, message }` for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CmdError {
    /// Error category.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

impl CmdError {
    /// Arbitrary code.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Unknown host id.
    pub fn no_such_host(host_id: &str) -> Self {
        Self::new("noSuchHost", format!("no host {host_id:?} (connect first)"))
    }

    /// Link-level failure (not connected, connection lost, dial failed).
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new("transport", message)
    }

    /// SSH / install / daemon start failure.
    pub fn bootstrap(message: impl Into<String>) -> Self {
        Self::new("bootstrap", message)
    }

    /// A proto error code.
    pub fn code(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(error_code_name(code), message)
    }
}

/// The JSON name of an `ErrorCode` (`"notFound"`, …).
pub fn error_code_name(code: ErrorCode) -> String {
    match serde_json::to_value(code) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "internal".to_string(),
    }
}

impl From<RpcError> for CmdError {
    fn from(e: RpcError) -> Self {
        Self::code(e.code, e.message)
    }
}

impl From<RequestError> for CmdError {
    fn from(e: RequestError) -> Self {
        match e {
            RequestError::Rpc(rpc) => rpc.into(),
            RequestError::ConnectionLost | RequestError::Closed => Self::transport(e.to_string()),
            RequestError::Unexpected(m) => Self::code(ErrorCode::Internal, m),
        }
    }
}

impl From<TransportError> for CmdError {
    fn from(e: TransportError) -> Self {
        Self::transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_code_and_message() {
        let e: CmdError = RequestError::Rpc(RpcError::not_found("pane 3")).into();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["code"], "notFound");
        assert!(v["message"].as_str().unwrap().contains("pane 3"));
        assert_eq!(
            CmdError::from(RequestError::ConnectionLost).code,
            "transport"
        );
        assert_eq!(
            error_code_name(ErrorCode::InvalidArgument),
            "invalidArgument"
        );
    }
}
