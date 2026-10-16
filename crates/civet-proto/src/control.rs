//! Control RPC (request/response on the control stream) and pushed events.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentStatus, AgentStatusSource};
use crate::ids::{PaneId, RepoId, RequestId, WorktreeId};
use crate::term::{ScrollbackPage, ScrollbackRequest, TermSize};

/// A repository known to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    /// Opaque id.
    pub id: RepoId,
    /// Display name (directory name by default).
    pub name: String,
    /// Absolute path of the main worktree (or bare repo dir) on the host.
    pub path: String,
    /// Default branch (`main`, `master`...) if it could be determined.
    pub default_branch: Option<String>,
}

/// A git worktree of a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    /// Opaque id (the canonical path).
    pub id: WorktreeId,
    /// Owning repository.
    pub repo: RepoId,
    /// Absolute path on the host.
    pub path: String,
    /// Checked-out branch short name; None if detached.
    pub branch: Option<String>,
    /// HEAD commit sha (hex), None for an unborn branch.
    pub head: Option<String>,
    /// True for the repository's main worktree.
    pub is_main: bool,
    /// True if the worktree is locked (`git worktree lock`).
    pub locked: bool,
}

/// State of a [`PullRequestInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestState {
    /// Open and ready for review.
    Open,
    /// Open as a draft.
    Draft,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

/// A GitHub pull request whose head is a branch of the repository itself
/// (not a fork), as `gh pr list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestInfo {
    /// PR number.
    pub number: u64,
    /// Head branch name.
    pub branch: String,
    /// Title.
    pub title: String,
    /// Web URL.
    pub url: String,
    /// State.
    pub state: PullRequestState,
}

/// Kind of a [`FileEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    /// Regular file (or anything else that is neither a directory nor a
    /// symlink: fifo, socket...).
    File,
    /// Directory.
    Dir,
    /// Symbolic link (not followed).
    Symlink,
}

/// One entry of a directory listing ([`Request::ListFiles`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// File name (no directory part).
    pub name: String,
    /// Kind, from `lstat`.
    pub kind: FileKind,
    /// Size in bytes (`lstat`); 0 for directories.
    pub size: u64,
    /// True if git ignores it (only listed when ignored entries are asked
    /// for).
    pub ignored: bool,
}

/// One directory level of a worktree ([`Response::Files`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    /// Directories first, then by name (case-insensitive).
    pub entries: Vec<FileEntry>,
    /// True if the directory had more than [`crate::MAX_DIR_ENTRIES`]
    /// entries and the rest were left out.
    pub truncated: bool,
}

/// Read-only preview of a file ([`Response::FilePreview`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    /// Size of the whole file in bytes.
    pub size: u64,
    /// True if the file looks binary (a NUL byte in its first 8 KiB);
    /// `text` is then empty.
    pub binary: bool,
    /// True if only the first `max_bytes` were read.
    pub truncated: bool,
    /// Contents as UTF-8 (invalid sequences replaced).
    pub text: String,
}

/// Every file of a worktree, for quick-open ([`Response::AllFiles`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    /// Paths relative to the worktree root (`/`-separated), sorted.
    pub paths: Vec<String>,
    /// True if there were more than [`crate::MAX_ALL_FILES`].
    pub truncated: bool,
}

/// Program + argv for a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    /// Executable (looked up in PATH if not absolute).
    pub program: String,
    /// Arguments (not including the program).
    pub args: Vec<String>,
}

impl CommandSpec {
    /// Convenience constructor.
    pub fn new<I, S>(program: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

/// Parameters for [`Request::SpawnPane`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnPaneRequest {
    /// Worktree the pane belongs to (used for grouping in the sidebar and as
    /// the default cwd).
    pub worktree: Option<WorktreeId>,
    /// Working directory; defaults to the worktree path, else `$HOME`.
    pub cwd: Option<String>,
    /// Command to run; None = the user's login shell.
    pub command: Option<CommandSpec>,
    /// Extra environment variables (added to the daemon's environment;
    /// `TERM=xterm-256color` and `CIVET_PANE_ID` are always set).
    pub env: Vec<(String, String)>,
    /// Initial terminal size.
    pub size: TermSize,
    /// Optional display title.
    pub title: Option<String>,
    /// Agent kind hint for status detection, e.g. `"claude"`, `"codex"`, or
    /// None for a plain shell / unknown CLI.
    pub agent: Option<String>,
}

/// Parameters for [`Request::LaunchAgent`]: create a new worktree (new
/// branch) and start an agent CLI in it, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchAgentRequest {
    /// Repository to branch from.
    pub repo: RepoId,
    /// New branch name (also the default worktree directory name, with `/`
    /// replaced by `-`). Must not exist yet, unless it still points at the
    /// base and is not checked out anywhere (then it is reused).
    pub branch: String,
    /// Rev to branch from; None = the repo's default branch (else HEAD).
    pub base: Option<String>,
    /// Worktree directory; None = daemon's default layout (see
    /// [`Request::CreateWorktree`]).
    pub worktree_path: Option<String>,
    /// Agent CLI to run, e.g. `claude` / `codex` with args. Run through the
    /// user's login shell (`$SHELL -lc`) so PATH from shell rc files applies.
    pub command: CommandSpec,
    /// Extra environment variables (see [`SpawnPaneRequest::env`]).
    pub env: Vec<(String, String)>,
    /// Initial terminal size.
    pub size: TermSize,
    /// Display title; None = branch name.
    pub title: Option<String>,
    /// Agent kind hint (`"claude"`, `"codex"`, ...) for status detection.
    pub agent: Option<String>,
}

/// Snapshot of a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneInfo {
    /// Pane id.
    pub id: PaneId,
    /// Owning worktree, if any.
    pub worktree: Option<WorktreeId>,
    /// Working directory at spawn time.
    pub cwd: String,
    /// argv (program first).
    pub command: Vec<String>,
    /// Current title (from OSC 0/2 or the spawn request).
    pub title: String,
    /// Current size.
    pub size: TermSize,
    /// Agent kind hint from the spawn request.
    pub agent: Option<String>,
    /// Current agent status.
    pub status: AgentStatus,
    /// Exit code once the process has exited; None while running (or if
    /// killed by a signal, in which case `exited` is still true).
    pub exit_code: Option<i32>,
    /// True once the process has exited. The pane (and its screen and
    /// scrollback) stays readable until killed.
    pub exited: bool,
    /// Creation time, ms since epoch.
    pub created_at_ms: u64,
    /// Current screen state number.
    pub state: u64,
}

/// Signal used by [`Request::KillPane`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KillSignal {
    /// SIGHUP (default; what closing a terminal sends).
    #[default]
    Hangup,
    /// SIGTERM.
    Terminate,
    /// SIGKILL.
    Kill,
}

/// Result of [`Request::SubscribePane`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeInfo {
    /// Pane.
    pub pane: PaneId,
    /// Server's current state number.
    pub state: u64,
    /// Current size.
    pub size: TermSize,
    /// Current `history_lines` (see [`crate::ScreenUpdate`]).
    pub history_lines: u64,
    /// Highest input seq from this client already applied; the client resends
    /// anything after it and continues numbering from `last_input_seq + 1`.
    pub last_input_seq: u64,
}

/// Daemon metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    /// `civetd` version.
    pub daemon_version: String,
    /// Protocol version.
    pub proto_version: u32,
    /// Process id.
    pub pid: u32,
    /// Daemon instance id (see [`crate::ServerHello::instance_id`]).
    pub instance_id: u64,
    /// Start time, ms since epoch.
    pub started_at_ms: u64,
    /// Host name.
    pub hostname: String,
}

/// Control requests, client -> daemon. Append-only (see crate docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Request {
    /// Liveness / RTT probe. -> [`Response::Pong`]
    Ping {
        /// Echoed back.
        nonce: u64,
    },
    /// -> [`Response::DaemonInfo`]
    GetDaemonInfo,
    /// -> [`Response::Repos`]
    ListRepos,
    /// Register a repository by path on the host. -> [`Response::Repo`]
    AddRepo {
        /// Path to any directory inside the repo (or a bare repo).
        path: String,
    },
    /// -> [`Response::Worktrees`]
    ListWorktrees {
        /// Repository.
        repo: RepoId,
    },
    /// `git worktree add`. -> [`Response::Worktree`]
    CreateWorktree {
        /// Repository.
        repo: RepoId,
        /// Branch to check out in the new worktree.
        branch: String,
        /// If set, create `branch` from this rev (branch/sha); if None,
        /// `branch` must already exist (or is created from HEAD if missing).
        base: Option<String>,
        /// Target directory; None = daemon's default layout
        /// (`<repo-parent>/<repo-name>-worktrees/<branch>`).
        path: Option<String>,
    },
    /// `git worktree remove`. -> [`Response::Ok`]
    RemoveWorktree {
        /// Worktree.
        worktree: WorktreeId,
        /// Remove even if dirty.
        force: bool,
    },
    /// Start a PTY. -> [`Response::PaneSpawned`]
    SpawnPane(SpawnPaneRequest),
    /// -> [`Response::Ok`]
    ResizePane {
        /// Pane.
        pane: PaneId,
        /// New size.
        size: TermSize,
    },
    /// Signal the process and drop the pane. -> [`Response::Ok`]
    KillPane {
        /// Pane.
        pane: PaneId,
        /// Signal.
        signal: KillSignal,
    },
    /// -> [`Response::Panes`]
    ListPanes,
    /// Start receiving [`crate::ScreenUpdate`]s for a pane.
    /// -> [`Response::Subscribed`]; the daemon then sends a first update that
    /// is a `Diff` from `known_state` if it still holds that snapshot for this
    /// client, else `Full`. Sending it again for an already-subscribed pane
    /// resets the baseline the same way (use `known_state: None` to force a
    /// `Full`, e.g. after the client dropped a `Diff` whose `base_state` did
    /// not match). The first update may arrive before the response.
    SubscribePane {
        /// Pane.
        pane: PaneId,
        /// Last state the client rendered (from a previous connection), if any.
        known_state: Option<u64>,
    },
    /// Stop screen updates for a pane. -> [`Response::Ok`]
    UnsubscribePane {
        /// Pane.
        pane: PaneId,
    },
    /// Scrollback over the control stream (alternative to a dedicated
    /// scrollback stream). -> [`Response::Scrollback`]
    FetchScrollback(ScrollbackRequest),
    /// Override a pane's agent status (e.g. user dismissed "done").
    /// -> [`Response::Ok`]
    SetAgentStatus {
        /// Pane.
        pane: PaneId,
        /// New status.
        status: AgentStatus,
    },
    /// Create a worktree on a new branch and spawn an agent in it.
    /// -> [`Response::AgentLaunched`]. If the spawn fails, the worktree is
    /// left in place and the error is returned (code `Spawn`).
    LaunchAgent(LaunchAgentRequest),
    /// Unregister a repository (does not touch files or worktrees).
    /// -> [`Response::Ok`]
    RemoveRepo {
        /// Repository.
        repo: RepoId,
    },
    /// Store a file the user dropped on a pane (e.g. an image for an agent)
    /// in a fresh daemon-side temp directory. -> [`Response::FileUploaded`].
    /// Clients send it only to daemons of at least
    /// [`crate::UPLOAD_FILE_MIN_DAEMON_VERSION`]; at most
    /// [`crate::MAX_UPLOAD_BYTES`] of data.
    UploadFile {
        /// Original file name (basename only; the daemon sanitizes it).
        name: String,
        /// File contents.
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// The repository's recent pull requests (`gh pr list` on the host,
    /// newest first; fork PRs left out). -> [`Response::PullRequests`].
    /// Clients send it only to daemons of at least
    /// [`crate::LIST_PULL_REQUESTS_MIN_DAEMON_VERSION`].
    ListPullRequests {
        /// Repository.
        repo: RepoId,
    },
    /// One directory level of a worktree, gitignored entries left out
    /// unless `include_ignored` (`.git` never listed). -> [`Response::Files`].
    /// Paths are relative to the worktree root and may not leave it
    /// (`..`, absolute paths and symlinks out are `InvalidArgument`).
    /// Clients send it only to daemons of at least
    /// [`crate::LIST_FILES_MIN_DAEMON_VERSION`] (also [`Request::ReadFile`],
    /// [`Request::ListAllFiles`]).
    ListFiles {
        /// Worktree.
        worktree: WorktreeId,
        /// Directory relative to the worktree root (`""` = the root).
        dir: String,
        /// Also list (and flag) gitignored entries.
        include_ignored: bool,
    },
    /// The start of a file, for a read-only preview.
    /// -> [`Response::FilePreview`]. Same path rules as
    /// [`Request::ListFiles`].
    ReadFile {
        /// Worktree.
        worktree: WorktreeId,
        /// File relative to the worktree root.
        path: String,
        /// Read at most this much (capped at
        /// [`crate::MAX_FILE_PREVIEW_BYTES`]).
        max_bytes: u64,
    },
    /// Every tracked and untracked, not ignored file of a worktree (`git
    /// ls-files -co --exclude-standard`), for quick-open.
    /// -> [`Response::AllFiles`]
    ListAllFiles {
        /// Worktree.
        worktree: WorktreeId,
    },
}

/// Successful responses. Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Response {
    /// Generic success with no payload.
    Ok,
    /// Reply to [`Request::Ping`].
    Pong {
        /// Echo of the request nonce.
        nonce: u64,
        /// Daemon wall clock, ms since epoch.
        server_time_ms: u64,
    },
    /// Reply to [`Request::GetDaemonInfo`].
    DaemonInfo(DaemonInfo),
    /// Reply to [`Request::ListRepos`].
    Repos(Vec<RepoInfo>),
    /// Reply to [`Request::AddRepo`].
    Repo(RepoInfo),
    /// Reply to [`Request::ListWorktrees`].
    Worktrees(Vec<WorktreeInfo>),
    /// Reply to [`Request::CreateWorktree`].
    Worktree(WorktreeInfo),
    /// Reply to [`Request::SpawnPane`].
    PaneSpawned(PaneInfo),
    /// Reply to [`Request::ListPanes`].
    Panes(Vec<PaneInfo>),
    /// Reply to [`Request::SubscribePane`].
    Subscribed(SubscribeInfo),
    /// Reply to [`Request::FetchScrollback`].
    Scrollback(ScrollbackPage),
    /// Reply to [`Request::LaunchAgent`].
    AgentLaunched {
        /// The new worktree.
        worktree: WorktreeInfo,
        /// The agent's pane.
        pane: PaneInfo,
    },
    /// Reply to [`Request::UploadFile`].
    FileUploaded {
        /// Absolute path of the stored file on the daemon's host.
        path: String,
    },
    /// Reply to [`Request::ListPullRequests`].
    PullRequests(Vec<PullRequestInfo>),
    /// Reply to [`Request::ListFiles`].
    Files(DirListing),
    /// Reply to [`Request::ReadFile`].
    FilePreview(FilePreview),
    /// Reply to [`Request::ListAllFiles`].
    AllFiles(FileList),
}

/// Error category for a failed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// Unknown pane / repo / worktree.
    NotFound,
    /// Malformed or contradictory arguments.
    InvalidArgument,
    /// Already exists (repo registered, branch/worktree exists...).
    AlreadyExists,
    /// git operation failed.
    Git,
    /// Spawning the process / PTY failed.
    Spawn,
    /// Filesystem or other I/O failure.
    Io,
    /// Not supported by this daemon.
    Unsupported,
    /// Anything else.
    Internal,
}

/// Failed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcError {
    /// Category.
    pub code: ErrorCode,
    /// Human-readable message.
    pub message: String,
}

impl RpcError {
    /// Convenience constructor.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `NotFound` shortcut.
    pub fn not_found(what: impl fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found"))
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    /// Client-chosen id, echoed in the response.
    pub id: RequestId,
    /// The request.
    pub request: Request,
}

/// Response envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    /// Id of the request this answers.
    pub id: RequestId,
    /// Outcome.
    pub result: Result<Response, RpcError>,
}

/// Agent status transition for a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusEvent {
    /// Pane.
    pub pane: PaneId,
    /// New status.
    pub status: AgentStatus,
    /// Previous status.
    pub previous: AgentStatus,
    /// Which detector fired.
    pub source: AgentStatusSource,
    /// Optional detail (notification text from OSC 9/777, hook message...).
    pub message: Option<String>,
    /// When, ms since epoch.
    pub at_ms: u64,
}

/// Events pushed by the daemon to every authenticated client. Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    /// A pane was created (by any client).
    PaneCreated(PaneInfo),
    /// A pane's process exited.
    PaneExited {
        /// Pane.
        pane: PaneId,
        /// Exit code, None if killed by a signal.
        exit_code: Option<i32>,
    },
    /// A pane was removed (killed); its id is now invalid.
    PaneClosed {
        /// Pane.
        pane: PaneId,
    },
    /// Title changed (OSC 0/2).
    PaneTitle {
        /// Pane.
        pane: PaneId,
        /// New title.
        title: String,
    },
    /// Pane was resized (by any client).
    PaneResized {
        /// Pane.
        pane: PaneId,
        /// New size.
        size: TermSize,
    },
    /// Agent status changed.
    AgentStatus(AgentStatusEvent),
    /// BEL received.
    Bell {
        /// Pane.
        pane: PaneId,
    },
    /// The set of repos changed.
    ReposChanged,
    /// The worktrees of a repo changed.
    WorktreesChanged {
        /// Repository.
        repo: RepoId,
    },
    /// The program in a pane set the clipboard with OSC 52 (write only: the
    /// daemon never answers OSC 52 queries). Sent only to clients that are
    /// subscribed to the pane and new enough to decode it (client version at
    /// least [`crate::CLIPBOARD_EVENT_MIN_CLIENT_VERSION`]); never replayed.
    /// `text` is at most [`crate::MAX_CLIPBOARD_BYTES`] bytes.
    Clipboard {
        /// Pane.
        pane: PaneId,
        /// Decoded clipboard text.
        text: String,
    },
}

impl Event {
    /// Pane the event concerns, if any.
    pub fn pane(&self) -> Option<PaneId> {
        match self {
            Event::PaneCreated(info) => Some(info.id),
            Event::PaneExited { pane, .. }
            | Event::PaneClosed { pane }
            | Event::PaneTitle { pane, .. }
            | Event::PaneResized { pane, .. }
            | Event::Bell { pane }
            | Event::Clipboard { pane, .. } => Some(*pane),
            Event::AgentStatus(e) => Some(e.pane),
            Event::ReposChanged | Event::WorktreesChanged { .. } => None,
        }
    }
}
