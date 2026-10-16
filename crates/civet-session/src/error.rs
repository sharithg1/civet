//! Error type of the session manager and its mapping to RPC errors.

use civet_proto::{ErrorCode, RpcError};

/// Errors returned by [`crate::SessionManager`].
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Unknown pane, repo or worktree.
    #[error("{0} not found")]
    NotFound(String),
    /// Malformed or contradictory arguments.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Repo already registered, branch or worktree already exists.
    #[error("already exists: {0}")]
    AlreadyExists(String),
    /// A git command failed (message includes git's trimmed stderr).
    #[error("git: {0}")]
    Git(String),
    /// Spawning the PTY or process failed.
    #[error("spawn failed: {0}")]
    Spawn(String),
    /// Filesystem or other I/O failure.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Not possible for this target (e.g. git or a pane in a folder
    /// project).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Backpressure: the pane's PTY input queue is full (the process is not
    /// reading). The input was NOT applied or recorded; wait with
    /// `SessionManager::wait_input_capacity` and retry the same message.
    #[error("busy: {0}")]
    Busy(String),
}

impl SessionError {
    /// The RPC error category for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            SessionError::NotFound(_) => ErrorCode::NotFound,
            SessionError::InvalidArgument(_) => ErrorCode::InvalidArgument,
            SessionError::AlreadyExists(_) => ErrorCode::AlreadyExists,
            SessionError::Git(_) => ErrorCode::Git,
            SessionError::Spawn(_) => ErrorCode::Spawn,
            SessionError::Io(_) => ErrorCode::Io,
            SessionError::Unsupported(_) => ErrorCode::Unsupported,
            SessionError::Busy(_) => ErrorCode::Internal,
        }
    }
}

impl From<&SessionError> for RpcError {
    fn from(e: &SessionError) -> Self {
        RpcError::new(e.code(), e.to_string())
    }
}

impl From<SessionError> for RpcError {
    fn from(e: SessionError) -> Self {
        RpcError::from(&e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codes() {
        let e = SessionError::NotFound("pane-3".into());
        let rpc: RpcError = (&e).into();
        assert_eq!(rpc.code, ErrorCode::NotFound);
        assert_eq!(rpc.message, "pane-3 not found");
        let rpc: RpcError = SessionError::Io(std::io::Error::other("x")).into();
        assert_eq!(rpc.code, ErrorCode::Io);
        let rpc: RpcError = SessionError::Git("bad".into()).into();
        assert_eq!(rpc.code, ErrorCode::Git);
    }
}
