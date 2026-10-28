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

/// What a registered project is ([`ProjectInfo`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectKind {
    /// A git repository: worktrees, sessions, changes.
    #[default]
    Git,
    /// A plain directory (not in a git repository). It has exactly one
    /// [`WorktreeInfo`], its root (`is_main`, no branch or head), whose
    /// files can be browsed and changed and which panes can run in; git
    /// requests and new worktrees in it are `Unsupported`. Once the
    /// directory holds a `.git` of its own, listing projects re-registers
    /// it as a [`ProjectKind::Git`] (a new RepoId, the same worktree id).
    Folder,
}

/// A registered project: a repository or a plain folder
/// ([`Request::ListProjects`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// Id, name and path (`default_branch` is None for a folder).
    pub repo: RepoInfo,
    /// Repository or folder.
    pub kind: ProjectKind,
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

/// One change to a worktree's files ([`Request::FileOp`]). Paths follow the
/// rules of [`Request::ListFiles`], and none may name `.git` or anything
/// inside it. Nothing is overwritten: a target that already exists is
/// `AlreadyExists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FileOp {
    /// Create an empty file (its directory must exist).
    CreateFile {
        /// New file.
        path: String,
    },
    /// Create a directory (its parent must exist).
    CreateDir {
        /// New directory.
        path: String,
    },
    /// Rename or move a file or directory (a symlink itself, not its
    /// target). A case-only rename of the same file is allowed.
    Move {
        /// Existing entry.
        from: String,
        /// New path.
        to: String,
    },
    /// Copy a file or directory tree (symlinks copied as symlinks, `.git`
    /// entries left out).
    Copy {
        /// Existing entry.
        from: String,
        /// New path.
        to: String,
    },
    /// Move a file or directory to the daemon's trash (under its state
    /// directory, not the worktree); [`FileOpResult::trash_id`] restores it.
    /// Old trash is purged after a while.
    Trash {
        /// Existing entry.
        path: String,
    },
    /// Put a trashed entry back at its old path (which must be free).
    Restore {
        /// From the [`FileOp::Trash`] result.
        trash_id: String,
    },
    /// Create a file with these contents (e.g. dropped from Finder), at
    /// most [`crate::MAX_UPLOAD_BYTES`].
    Write {
        /// New file.
        path: String,
        /// Contents.
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
}

/// Outcome of a [`Request::FileOp`] ([`Response::FileOpDone`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOpResult {
    /// The path the op created or changed, relative to the worktree root
    /// (for [`FileOp::Restore`]: where the entry is back).
    pub path: String,
    /// Set by [`FileOp::Trash`]: pass it to [`FileOp::Restore`].
    pub trash_id: Option<String>,
}

/// A slice of a file ([`Response::FileChunk`]), for downloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChunk {
    /// Size of the whole file in bytes.
    pub size: u64,
    /// Bytes from the requested offset; empty at the end of the file.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// One subdirectory in a [`HostDirListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDir {
    /// Directory name (no parent part).
    pub name: String,
    /// True if it holds a `.git` (a repository or worktree root) or is a
    /// bare repository.
    pub git_repo: bool,
}

/// Subdirectories of a host directory, for path completion
/// ([`Response::Dirs`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDirListing {
    /// The directory listed: absolute, `~` expanded (not canonicalized).
    pub path: String,
    /// Sorted by name (case-insensitive); hidden ones included. Symlinks
    /// to directories count as directories.
    pub dirs: Vec<HostDir>,
    /// True if there were more than [`crate::MAX_DIR_ENTRIES`].
    pub truncated: bool,
}

/// How a file changed, in a [`GitFileChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GitChangeKind {
    /// Content (or mode) changed.
    Modified,
    /// New file (staged, or intent-to-add).
    Added,
    /// Deleted.
    Deleted,
    /// Renamed from `old_path`.
    Renamed,
    /// Copied from `old_path`.
    Copied,
    /// File type changed (regular file, symlink, submodule).
    TypeChanged,
    /// Not tracked by git (and not ignored).
    Untracked,
    /// Unmerged (merge / rebase conflict).
    Conflicted,
}

/// One changed path of a worktree, as [`Request::GitStatus`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    /// Path relative to the worktree root (`/`-separated).
    pub path: String,
    /// Source path of a rename or copy.
    pub old_path: Option<String>,
    /// Kind of change.
    pub kind: GitChangeKind,
    /// Added lines; None if binary or not counted.
    pub additions: Option<u64>,
    /// Deleted lines; None if binary or not counted.
    pub deletions: Option<u64>,
}

/// A worktree's `git status`: index vs. HEAD (`staged`), working tree vs.
/// index (`unstaged`), untracked and unmerged files. A path can be both
/// staged and unstaged. Sorted by path within each list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusInfo {
    /// Changes in the index.
    pub staged: Vec<GitFileChange>,
    /// Changes in the working tree not yet staged.
    pub unstaged: Vec<GitFileChange>,
    /// Untracked files (each file, not just directories).
    pub untracked: Vec<GitFileChange>,
    /// Unmerged paths.
    pub conflicted: Vec<GitFileChange>,
    /// True if entries beyond [`crate::MAX_GIT_STATUS_ENTRIES`] were left out.
    pub truncated: bool,
}

/// One file's diff, as [`Request::GitDiff`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffInfo {
    /// Requested path.
    pub path: String,
    /// git considers the file binary (`diff` is empty).
    pub binary: bool,
    /// The patch is over [`crate::MAX_GIT_DIFF_BYTES`] or
    /// [`crate::MAX_GIT_DIFF_LINES`] changed lines (`diff` is empty).
    pub too_large: bool,
    /// Added lines; None if binary.
    pub additions: Option<u64>,
    /// Deleted lines; None if binary.
    pub deletions: Option<u64>,
    /// `git diff` output for the file (headers and hunks); empty if there
    /// is no change, or if `binary` / `too_large`.
    pub diff: String,
}

/// A worktree's branch and how it relates to its upstream, as
/// [`Request::GitBranch`] reports it (the Changes tab header).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchInfo {
    /// Checked-out branch; None if HEAD is detached.
    pub branch: Option<String>,
    /// HEAD commit; None on an unborn branch.
    pub head: Option<String>,
    /// Remote-tracking branch of the upstream (`origin/feat`); None without
    /// one, or while it is configured but not pushed yet.
    pub upstream: Option<String>,
    /// Commits on HEAD that the upstream lacks (0 without an upstream).
    pub ahead: u64,
    /// Commits on the upstream that HEAD lacks.
    pub behind: u64,
    /// HEAD has a parent and is not on its upstream, so
    /// [`GitAction::UndoCommit`] may undo it.
    pub can_undo_commit: bool,
    /// Stash entries made on this branch (the only ones
    /// [`GitAction::StashPop`] pops; the stash list is shared by every
    /// worktree of the repository).
    pub stashes: u64,
    /// The repository has a remote to push to.
    pub has_remote: bool,
}

/// A blob in the repository's object database and the mode to restore it
/// with (`0o100644`, `0o100755` or `0o120000` for a symlink).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlobRef {
    /// Object id (hex).
    pub oid: String,
    /// git file mode.
    pub mode: u32,
}

/// One path's state before a [`GitAction::Discard`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiscardEntry {
    /// Path relative to the worktree root.
    pub path: String,
    /// Working-tree content; None if the file did not exist.
    pub file: Option<GitBlobRef>,
    /// The discard also reset the index for this path, which held `index`
    /// (None: the path was not in the index).
    pub restore_index: bool,
    /// Index entry before the discard (meaningful if `restore_index`).
    pub index: Option<GitBlobRef>,
    /// The file was untracked (the discard deleted it).
    pub untracked: bool,
}

/// What a [`GitAction::Discard`] overwrote, so [`GitAction::RestoreDiscarded`]
/// can put it back. Contents are written to the object database
/// (`git hash-object -w`) before anything is touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiscardSnapshot {
    /// One entry per discarded path.
    pub entries: Vec<GitDiscardEntry>,
}

/// Which side of a merge conflict to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictSide {
    /// The checked-out branch (`git checkout --ours`).
    Ours,
    /// The branch being merged in (`git checkout --theirs`).
    Theirs,
}

/// A git write operation on a worktree ([`Request::GitAction`]). Paths are
/// relative to the worktree root, may not leave it and never name `.git`.
/// Append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GitAction {
    /// `git add -A` the paths (also marks conflicts resolved).
    Stage {
        /// Paths.
        paths: Vec<String>,
    },
    /// Reset the paths' index entries to HEAD (`git reset`).
    Unstage {
        /// Paths (both sides of a rename).
        paths: Vec<String>,
    },
    /// Throw away changes: the working tree goes back to the index, or with
    /// `staged` index and working tree back to HEAD; untracked files are
    /// deleted. -> [`Response::GitDiscarded`] with what was overwritten.
    /// Unmerged paths are refused.
    Discard {
        /// Paths (both sides of a staged rename).
        paths: Vec<String>,
        /// Also discard staged changes.
        staged: bool,
    },
    /// Put back what a [`GitAction::Discard`] overwrote.
    RestoreDiscarded(GitDiscardSnapshot),
    /// `git commit`; with `all`, everything (untracked files too) is staged
    /// first. With `amend`, an empty message keeps the last one.
    Commit {
        /// Commit message.
        message: String,
        /// `--amend`.
        amend: bool,
        /// Stage all changes first.
        all: bool,
    },
    /// Resolve conflicts by taking one side's version of each path (or
    /// removing the path if that side deleted it), and stage the result.
    AcceptConflict {
        /// Unmerged paths.
        paths: Vec<String>,
        /// Side to keep.
        side: ConflictSide,
    },
    /// `git push`; a branch without an upstream is pushed to the remote of
    /// the same name and tracks it (`-u`).
    Push,
    /// `git pull` from the upstream (a merge unless `pull.rebase` /
    /// `pull.ff` say otherwise).
    Pull,
    /// `git fetch`.
    Fetch,
    /// `git reset --soft HEAD~1`, only while HEAD is `head` and not on the
    /// upstream (see [`GitBranchInfo::can_undo_commit`]).
    UndoCommit {
        /// The commit the client means to undo.
        head: String,
    },
    /// `git stash push -u` of every change.
    StashAll,
    /// Apply and drop the newest stash entry made on this branch.
    StashPop,
}

/// Per-project settings for new worktrees ([`Request::GetWorktreeTemplate`]).
/// Every field is optional: None = not set here (the next source applies),
/// while an empty value is set and means "nothing". The sources, highest
/// first: the template saved on the host ([`Request::SetWorktreeTemplate`],
/// from the desktop's project settings), then `.civet/worktree.toml` in
/// the repository's main checkout (committed and shared). See
/// `docs/WORKTREE_TEMPLATES.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeTemplate {
    /// Script run in a "Setup" pane in every new worktree ([`Request::SetupWorktree`]):
    /// `sh`, unless it starts with a `#!` line.
    pub setup: Option<String>,
    /// Paths relative to the main checkout copied into a new worktree
    /// (files or directories; `*` and `?` match within one path component).
    /// Files the worktree already has are kept.
    pub copy: Option<Vec<String>>,
    /// Command line of the first agent (e.g. `claude --model opus`).
    pub agent: Option<String>,
    /// Default first prompt of the first agent (prepended to a handoff's brief).
    pub prompt: Option<String>,
}

/// The template of a repository and where its fields come from (reply to
/// [`Request::GetWorktreeTemplate`] and [`Request::SetWorktreeTemplate`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeTemplateInfo {
    /// Repository.
    pub repo: RepoId,
    /// Saved on the host (the desktop's project settings).
    pub host: WorktreeTemplate,
    /// Absolute path of the repository file (`<main>/.civet/worktree.toml`).
    pub file_path: String,
    /// Parsed repository file; None if it does not exist or does not parse.
    pub file: Option<WorktreeTemplate>,
    /// Why the repository file could not be read or parsed.
    pub file_error: Option<String>,
    /// What new worktrees use: `host` fields over `file` fields.
    pub effective: WorktreeTemplate,
}

/// What [`Request::SetupWorktree`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSetup {
    /// The effective template applied (its `agent` and `prompt` are for
    /// the caller, who starts the agent).
    pub template: WorktreeTemplate,
    /// Paths copied from the main checkout (relative).
    pub copied: Vec<String>,
    /// Problems that did not stop the setup (a listed file missing in the
    /// main checkout, a copy that failed, a bad repository file).
    pub warnings: Vec<String>,
    /// `CIVET_PORT` of the worktree (see [`crate::ENV_PORT`]).
    pub port: Option<u16>,
    /// The pane running the setup script, if the template has one.
    pub setup_pane: Option<PaneInfo>,
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
    /// Git repositories only (folder projects are left out, so clients that
    /// predate them never see one). -> [`Response::Repos`]
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
    /// The worktree's changes (`git status`, line counts from
    /// `git diff --numstat`). -> [`Response::GitStatus`]. Clients send it
    /// only to daemons of at least [`crate::GIT_STATUS_MIN_DAEMON_VERSION`].
    GitStatus {
        /// Worktree.
        worktree: WorktreeId,
    },
    /// One file's unified diff: index vs. HEAD if `staged`, else working
    /// tree vs. index (an untracked file against nothing). -> [`Response::GitDiff`].
    /// Gated like [`Request::GitStatus`].
    GitDiff {
        /// Worktree.
        worktree: WorktreeId,
        /// Path relative to the worktree root (as in [`GitFileChange`]).
        path: String,
        /// Source path of a rename / copy, to diff the pair.
        old_path: Option<String>,
        /// Index vs. HEAD instead of working tree vs. index.
        staged: bool,
        /// Context lines around each hunk (`-U`).
        context_lines: u32,
    },
    /// The worktree's branch, upstream and ahead / behind counts.
    /// -> [`Response::GitBranch`]. Clients send it only to daemons of at
    /// least [`crate::GIT_ACTIONS_MIN_DAEMON_VERSION`] (also
    /// [`Request::GitAction`]).
    GitBranch {
        /// Worktree.
        worktree: WorktreeId,
    },
    /// Run a git write operation in the worktree. -> [`Response::Ok`], or
    /// [`Response::GitDiscarded`] for [`GitAction::Discard`]. git's own
    /// refusals (nothing to commit, push rejected, conflicts) are `Git`
    /// errors carrying its message.
    GitAction {
        /// Worktree.
        worktree: WorktreeId,
        /// What to do.
        action: GitAction,
    },
    /// Create, rename / move, copy, trash or restore worktree files.
    /// -> [`Response::FileOpDone`]. Clients send it only to daemons of at
    /// least [`crate::FILE_OPS_MIN_DAEMON_VERSION`] (also
    /// [`Request::ReadFileChunk`]).
    FileOp {
        /// Worktree.
        worktree: WorktreeId,
        /// The operation.
        op: FileOp,
    },
    /// Up to `len` bytes of a regular file from `offset` (capped at
    /// [`crate::MAX_FILE_CHUNK_BYTES`]), for downloading it whole.
    /// -> [`Response::FileChunk`]. Same path rules as [`Request::ReadFile`].
    ReadFileChunk {
        /// Worktree.
        worktree: WorktreeId,
        /// File relative to the worktree root.
        path: String,
        /// Start, in bytes.
        offset: u64,
        /// Most bytes wanted.
        len: u64,
    },
    /// Every registered project, repositories and folders.
    /// -> [`Response::Projects`]. Clients send it only to daemons of at
    /// least [`crate::PROJECTS_MIN_DAEMON_VERSION`] (also
    /// [`Request::AddProject`]); older ones only have
    /// [`Request::ListRepos`].
    ListProjects,
    /// Register the git repository containing `path`, like
    /// [`Request::AddRepo`], or, if `path` is in no git repository, the
    /// directory itself as a [`ProjectKind::Folder`].
    /// -> [`Response::Project`]
    AddProject {
        /// Any directory.
        path: String,
    },
    /// The subdirectories of any directory on the host, for completing a
    /// path (the desktop's "Add project" folder field). -> [`Response::Dirs`].
    /// Clients send it only to daemons of at least
    /// [`crate::LIST_DIRS_MIN_DAEMON_VERSION`].
    ListDirs {
        /// Absolute, or `~` / `~/...`; anything else is relative to the
        /// home directory.
        path: String,
    },
    /// CPU, memory, disk, network and GPU use of the host (see
    /// [`crate::stats`]). -> [`Response::HostStats`]. Clients send it only
    /// to daemons of at least [`crate::HOST_STATS_MIN_DAEMON_VERSION`].
    HostStats {
        /// Newest [`crate::StatsSample::seq`] the client has; only newer
        /// samples come back. None: the whole history.
        since: Option<u64>,
        /// Also list processes and the panes they belong to (costlier:
        /// the daemon samples processes only while someone asks for them).
        processes: bool,
    },
    /// The repository's worktree template and its sources.
    /// -> [`Response::WorktreeTemplate`]. Clients send it (and
    /// [`Request::SetWorktreeTemplate`], [`Request::SetupWorktree`]) only to
    /// daemons of at least [`crate::WORKTREE_TEMPLATE_MIN_DAEMON_VERSION`].
    GetWorktreeTemplate {
        /// Repository.
        repo: RepoId,
    },
    /// Replace the template saved on the host for a repository (an
    /// all-None template deletes it). -> [`Response::WorktreeTemplate`]
    SetWorktreeTemplate {
        /// Repository.
        repo: RepoId,
        /// New host template.
        template: WorktreeTemplate,
    },
    /// Apply the repository's template to a new worktree: copy the listed
    /// files from the main checkout, assign its `CIVET_PORT`, and start the
    /// setup script in a pane. The caller then starts the agent.
    /// -> [`Response::WorktreeSetUp`]
    SetupWorktree {
        /// The new worktree (not the main one).
        worktree: WorktreeId,
        /// Initial size of the setup pane.
        size: TermSize,
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
    /// Reply to [`Request::GitStatus`].
    GitStatus(GitStatusInfo),
    /// Reply to [`Request::GitDiff`].
    GitDiff(GitDiffInfo),
    /// Reply to [`Request::GitBranch`].
    GitBranch(GitBranchInfo),
    /// Reply to a [`GitAction::Discard`]: what to send back in
    /// [`GitAction::RestoreDiscarded`] to undo it.
    GitDiscarded(GitDiscardSnapshot),
    /// Reply to [`Request::FileOp`].
    FileOpDone(FileOpResult),
    /// Reply to [`Request::ReadFileChunk`].
    FileChunk(FileChunk),
    /// Reply to [`Request::ListProjects`].
    Projects(Vec<ProjectInfo>),
    /// Reply to [`Request::AddProject`].
    Project(ProjectInfo),
    /// Reply to [`Request::ListDirs`].
    Dirs(HostDirListing),
    /// Reply to [`Request::HostStats`].
    HostStats(crate::stats::HostStats),
    /// Reply to [`Request::GetWorktreeTemplate`] and
    /// [`Request::SetWorktreeTemplate`].
    WorktreeTemplate(WorktreeTemplateInfo),
    /// Reply to [`Request::SetupWorktree`].
    WorktreeSetUp(WorktreeSetup),
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
