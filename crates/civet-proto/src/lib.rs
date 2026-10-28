//! Wire protocol for Civet.
//!
//! This crate is the single source of truth for every byte that crosses the
//! network between the desktop client (or `civet-cli`) and the `civetd`
//! daemon. It has no networking code of its own: `civet-transport` moves the
//! frames, `civetd` / `civet-session` produce and consume the messages.
//!
//! # Layers
//!
//! * [`bootstrap`]: the single JSON line `civetd` prints on stdout when it is
//!   started over SSH ([`BootstrapInfo`]).
//! * [`hello`]: the first frame on the control stream ([`ClientHello`]) and the
//!   daemon's reply ([`HelloReply`]). Authenticates with the [`SessionKey`].
//! * [`messages`]: the steady-state envelopes [`ClientMessage`] and
//!   [`ServerMessage`], plus [`StreamOpen`], the header that identifies every
//!   QUIC stream after the control stream.
//! * [`control`]: control RPC ([`Request`] / [`Response`]) and pushed
//!   [`Event`]s.
//! * [`ports`]: listening-port updates and TCP port forwarding, each on
//!   their own QUIC streams.
//! * [`stats`]: host resource stats ([`HostStats`]), polled by the
//!   stats view.
//! * [`prompts`]: prompts the daemon types into agent panes (broadcast and
//!   queued follow-ups, [`QueuedPrompt`]).
//! * [`usage`]: agent token use and estimated spend ([`AgentUsage`]), read
//!   from the agents' session logs.
//! * [`term`]: terminal traffic: [`InputMessage`] (client keystrokes with
//!   sequence numbers), [`ScreenUpdate`] (server screen diffs as VT bytes),
//!   [`ScreenAck`], and scrollback paging.
//! * [`frame`]: length-prefixed framing (`u32` big-endian length + postcard
//!   payload), sync and async (tokio) helpers.
//!
//! # Encoding and versioning
//!
//! Every frame payload is [postcard](https://docs.rs/postcard). Postcard is not
//! self-describing: enum variants are encoded by index and struct fields by
//! position. Therefore, within one [`PROTOCOL_VERSION`]:
//!
//! * enum variants may only be **appended** (never reordered or removed);
//! * struct fields must never be added, removed or reordered — doing so
//!   requires bumping [`PROTOCOL_VERSION`].
//!
//! The hello frame starts with [`MAGIC`] and the protocol version so a peer of
//! any version can read those two fields ([`hello::peek_hello_version`]) and
//! reject a mismatch cleanly.
//!
//! The types also implement JSON-friendly serde (camelCase field names, hex
//! strings for keys) so the desktop app can hand them to its web UI unchanged.
//! Attributes that postcard cannot handle (`#[serde(tag = ...)]`,
//! `#[serde(untagged)]`, `#[serde(flatten)]`, `skip_serializing_if`) must not
//! be used in this crate.

pub mod agent;
pub mod bootstrap;
pub mod control;
pub mod error;
pub mod frame;
pub mod hello;
pub mod ids;
pub mod keys;
pub mod messages;
pub mod ports;
pub mod prompts;
pub mod stats;
pub mod term;
pub mod usage;

pub use agent::{
    AgentStatus, AgentStatusSource, MAX_STATUS_FILE_BYTES, MAX_STATUS_MESSAGE_CHARS,
    parse_status_file,
};
pub use bootstrap::{BOOTSTRAP_FORMAT, BootstrapInfo};
pub use control::{
    AgentStatusEvent, CheckState, CommandSpec, ConflictSide, ControlRequest, ControlResponse,
    CreatePullRequest, DaemonInfo, DirListing, ErrorCode, Event, FileChunk, FileEntry, FileKind,
    FileList, FileOp, FileOpResult, FilePreview, GitAction, GitBlobRef, GitBranchInfo,
    GitChangeKind, GitDiffInfo, GitDiscardEntry, GitDiscardSnapshot, GitFileChange, GitStatusInfo,
    HostDir, HostDirListing, KillSignal, LaunchAgentRequest, PaneInfo, ProjectInfo, ProjectKind,
    PullRequestCheck, PullRequestDetail, PullRequestInfo, PullRequestState, RepoInfo,
    RepoLifecycle, Request, Response, ReviewDecision, ReviewThread, RpcError, SpawnPaneRequest,
    StaleReason, SubscribeInfo, UpdateOutcome, UpdateStrategy, WorktreeInfo, WorktreeLifecycle,
    WorktreeSetup, WorktreeTemplate, WorktreeTemplateInfo, WorktreeUpdate,
};
pub use error::ProtoError;
pub use frame::{
    FrameDecoder, MAX_FRAME_LEN, MAX_HELLO_LEN, MAX_STREAM_OPEN_LEN, decode, encode, encode_frame,
};
pub use hello::{ClientHello, HelloReject, HelloReply, RejectReason, ServerHello};
pub use ids::{ClientId, PaneId, RepoId, RequestId, WorktreeId};
pub use keys::{CertFingerprint, ResumeToken, SessionKey};
pub use messages::{ClientMessage, ServerMessage, StreamOpen};
pub use ports::{ForwardReply, ListeningPort, MAX_PORT_COMMAND_CHARS, PortsUpdate};
pub use prompts::{MAX_PROMPT_BYTES, MAX_QUEUED_PROMPTS, PromptTrigger, QueuedPrompt};
pub use stats::{
    DiskSpace, GpuInfo, HostStats, MAX_STATS_COMMAND_CHARS, MAX_STATS_PROCESSES, MAX_STATS_SAMPLES,
    ProcessUsage, StatsSample,
};
pub use term::{
    ECHO_ACK_DELAY_MS, FULL_REPLAY_LINES, InputMessage, MAX_INFLIGHT_UPDATES, MAX_REPLAY_LINES,
    MAX_TERM_COLS, MAX_TERM_ROWS, ScreenAck, ScreenUpdate, ScrollbackLine, ScrollbackPage,
    ScrollbackQuery, ScrollbackRequest, TermSize, UpdateKind,
};
pub use usage::{
    AgentSessionUsage, AgentUsage, MAX_USAGE_SESSIONS, ModelPrice, TokenCounts, UsageAgent,
    UsageHour,
};

/// Wire protocol version. Bump on any incompatible change (see crate docs).
pub const PROTOCOL_VERSION: u32 = 1;

/// Oldest protocol version this build can talk to. Equal to
/// [`PROTOCOL_VERSION`] until we deliberately support a range.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Largest OSC 52 clipboard text the daemon forwards ([`Event::Clipboard`]);
/// bigger writes are dropped.
pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

/// Oldest client (`ClientHello::client_version`) that can decode
/// [`Event::Clipboard`]. The variant was appended within protocol 1; older
/// clients would fail to decode it and drop the connection, so the daemon
/// does not send it to them.
pub const CLIPBOARD_EVENT_MIN_CLIENT_VERSION: &str = "0.1.1";

/// Oldest daemon that understands [`Request::UploadFile`]. The variant was
/// appended within protocol 1; an older daemon fails to decode it and drops
/// the connection, so clients check `daemon_version` before sending it.
pub const UPLOAD_FILE_MIN_DAEMON_VERSION: &str = "0.1.4";

/// Oldest daemon that understands [`Request::ListPullRequests`] (appended
/// within protocol 1, gated like [`UPLOAD_FILE_MIN_DAEMON_VERSION`]).
pub const LIST_PULL_REQUESTS_MIN_DAEMON_VERSION: &str = "0.1.5";

/// Oldest daemon that understands [`Request::ListFiles`],
/// [`Request::ReadFile`] and [`Request::ListAllFiles`] (appended within
/// protocol 1, gated like [`UPLOAD_FILE_MIN_DAEMON_VERSION`]).
pub const LIST_FILES_MIN_DAEMON_VERSION: &str = "0.1.10";

/// Most entries in a [`DirListing`]; the rest of a huge directory is left
/// out (`truncated`).
pub const MAX_DIR_ENTRIES: usize = 10_000;

/// Largest [`Request::ReadFile`] preview (well under the frame limit).
pub const MAX_FILE_PREVIEW_BYTES: u64 = 1024 * 1024;

/// Most paths in a [`FileList`] (`truncated` beyond).
pub const MAX_ALL_FILES: usize = 50_000;

/// Oldest daemon that understands [`Request::GitStatus`] and
/// [`Request::GitDiff`] (appended within protocol 1 after the file requests).
pub const GIT_STATUS_MIN_DAEMON_VERSION: &str = "0.1.11";

/// Most entries a [`GitStatusInfo`] carries (all lists together).
pub const MAX_GIT_STATUS_ENTRIES: usize = 5000;

/// Largest patch a [`GitDiffInfo`] carries; bigger ones are `too_large`.
pub const MAX_GIT_DIFF_BYTES: usize = 1024 * 1024;

/// Most changed lines (added + deleted) a [`GitDiffInfo`] carries.
pub const MAX_GIT_DIFF_LINES: u64 = 20_000;

/// Oldest daemon that understands [`Request::GitBranch`] and
/// [`Request::GitAction`] (appended within protocol 1 after the git status
/// requests).
pub const GIT_ACTIONS_MIN_DAEMON_VERSION: &str = "0.1.14";

/// Oldest daemon that understands [`Request::FileOp`] and
/// [`Request::ReadFileChunk`] (appended within protocol 1 after the git
/// action requests).
pub const FILE_OPS_MIN_DAEMON_VERSION: &str = "0.1.15";

/// Oldest daemon that understands [`Request::ListProjects`] and
/// [`Request::AddProject`], i.e. folder projects (appended within protocol
/// 1 after the file op requests). Older daemons only add git repositories.
pub const PROJECTS_MIN_DAEMON_VERSION: &str = "0.1.17";

/// Oldest daemon that understands [`Request::ListDirs`] (appended within
/// protocol 1 after the project requests).
pub const LIST_DIRS_MIN_DAEMON_VERSION: &str = "0.1.18";

/// Oldest daemon that understands [`Request::HostStats`] (appended within
/// protocol 1 after [`Request::ListDirs`]).
pub const HOST_STATS_MIN_DAEMON_VERSION: &str = "0.2.1";

/// Oldest daemon that understands [`Request::WorktreeLifecycle`] and
/// [`Request::UpdateWorktree`] (appended within protocol 1 after
/// [`Request::HostStats`]).
pub const WORKTREE_LIFECYCLE_MIN_DAEMON_VERSION: &str = "0.4.1";

/// Oldest daemon that understands [`Request::GetWorktreeTemplate`],
/// [`Request::SetWorktreeTemplate`] and [`Request::SetupWorktree`]
/// (appended within protocol 1 after [`Request::UpdateWorktree`]).
pub const WORKTREE_TEMPLATE_MIN_DAEMON_VERSION: &str = "0.4.2";

/// Oldest daemon that understands [`Request::PullRequestDetail`] and
/// [`Request::CreatePullRequest`] (appended within protocol 1 after
/// [`Request::SetupWorktree`]).
pub const PULL_REQUEST_DETAIL_MIN_DAEMON_VERSION: &str = "0.4.3";

/// Longest [`ReviewThread::body`], in characters.
pub const MAX_REVIEW_COMMENT_CHARS: usize = 600;

/// Oldest daemon that understands [`Request::SendPrompt`] and the prompt
/// queue requests (appended within protocol 1 after
/// [`Request::CreatePullRequest`]).
pub const PROMPTS_MIN_DAEMON_VERSION: &str = "0.4.4";

/// Oldest client (`ClientHello::client_version`) that can decode
/// [`Event::PromptQueue`] (appended within protocol 1 after
/// [`Event::Clipboard`]; gated like [`CLIPBOARD_EVENT_MIN_CLIENT_VERSION`]).
pub const PROMPT_QUEUE_EVENT_MIN_CLIENT_VERSION: &str = "0.4.4";

/// Oldest daemon that understands [`Request::AgentUsage`] (appended within
/// protocol 1 after [`Request::ListQueuedPrompts`]).
pub const AGENT_USAGE_MIN_DAEMON_VERSION: &str = "0.4.5";

/// Largest [`Request::ReadFileChunk`] slice (headroom under the frame limit).
pub const MAX_FILE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Largest [`Request::UploadFile`] payload, and of a [`FileOp::Write`]
/// (headroom under the frame limit).
pub const MAX_UPLOAD_BYTES: usize = 15 * 1024 * 1024;

/// First four bytes of every [`ClientHello`]; lets the daemon reject
/// non-Civet traffic on its UDP port immediately.
pub const MAGIC: [u8; 4] = *b"CIVT";

/// ALPN protocol id used for the QUIC/TLS handshake.
pub const ALPN: &[u8] = b"civet/1";

/// Environment variable set in every pane: the pane id (decimal).
pub const ENV_PANE_ID: &str = "CIVET_PANE_ID";

/// Environment variable set in every pane: absolute path of the pane's agent
/// status hook file. Agent hooks write a status into it (format:
/// [`agent::parse_status_file`]); the daemon watches it.
pub const ENV_STATUS_FILE: &str = "CIVET_STATUS_FILE";

/// Environment variable set in every pane of a worktree: a TCP port picked
/// for it on the host, the first of a block of ten (`CIVET_PORT` …
/// `CIVET_PORT + 9`) that no other worktree is given. Stable for the
/// worktree's lifetime.
pub const ENV_PORT: &str = "CIVET_PORT";

/// Name of the per-user state directory under `$HOME` (`~/.civet`).
pub const STATE_DIR_NAME: &str = ".civet";

/// Environment variable that overrides the state directory (tests, multiple
/// daemons on one host).
pub const ENV_STATE_DIR: &str = "CIVET_STATE_DIR";

/// Milliseconds since the Unix epoch, used for timestamps in messages.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Returns true if a peer speaking `version` is compatible with this build.
pub fn is_compatible(version: u32) -> bool {
    (MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version)
}
