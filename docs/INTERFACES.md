# Civet crate interfaces

_Binding contract between crates. As of 2026-10-14, protocol version 1._

This document is the contract four implementers work from in parallel
(civet-transport, civet-term, civet-session, civet-desktop). If you need to
change something here, change this file in the same patch and say so in your
hand-off, because someone else is coding against it. See
[ARCHITECTURE.md](ARCHITECTURE.md) for why things are shaped this way.

Naming: the product is **Civet**. "Orca" means the existing Electron app at
`/Users/sharith/Desktop/repos/orca` (read-only reference). Env vars use the
`CIVET_` prefix. Per-user state lives in `~/.civet/` (override:
`CIVET_STATE_DIR`).

## Contents

0. Ground rules
1. civet-proto (done): what exists and how to use it
2. civet-transport
3. civet-term
4. civet-session
5. civetd (daemon, bootstrap client, civet-cli)
6. civet-desktop (Tauri commands, binary channel, events, prediction)
7. On-disk layout and environment variables
8. Constants cheat sheet

---

## 0. Ground rules

- **Workspace.** Rust 2024 edition, `rust-version = 1.85`, stable toolchain.
  Rust is not on PATH in agent shells: prefix commands with
  `source ~/.cargo/env &&`.
- **Dependencies** come only from `[workspace.dependencies]` in the root
  `Cargo.toml` (`foo.workspace = true`). Versions there are pinned and verified
  to resolve. If you need a new crate, add it to the root table (append under
  the right comment heading; do not edit other entries) and mention it in your
  hand-off. Available: tokio, tokio-util, futures, quinn 0.11, rustls 0.23
  (ring), rustls-pki-types, rcgen 0.14, sha2 0.11, getrandom, rand, subtle,
  serde, serde_json, serde_bytes, postcard, bytes, hex, thiserror, anyhow,
  tracing, tracing-subscriber, clap 4, alacritty_terminal 0.26, portable-pty
  0.9, crossterm 0.29, libc, notify 8, dirs 7, tempfile, proptest, tauri
  2.12, tauri-build 2.7, tauri-plugin-notification 2.7.
- **No git2/gix.** Repos and worktrees are managed by running the `git` CLI.
- **civet-desktop is not a default member.** `cargo check` at the root skips
  it; `cargo check --workspace` includes it. Until the desktop implementer adds
  Tauri, it stays a light lib. When Tauri is added, the aliases
  `cargo check-core` / `test-core` / `clippy-core` (in `.cargo/config.toml`)
  exclude it so the other crates stay fast to build.
- **Quality gate** for every crate:
  `cargo clippy -p <crate> --all-targets -- -D warnings` and
  `cargo test -p <crate>`. Workspace lints: `unsafe_code = "deny"`
  (use `libc` through a tiny `#[allow(unsafe_code)]` module only where
  unavoidable, e.g. `killpg`/`setsid`, with a `// SAFETY:` comment),
  `missing_debug_implementations = "warn"` (derive or hand-write `Debug` on
  every public type).
- **Errors.** Library crates use `thiserror` enums (defined below). `anyhow`
  only in binaries and tests.
- **Logging.** `tracing` everywhere. Never log session keys, resume tokens or
  input bytes (input may contain passwords). Log lengths instead.
- **Async.** tokio multi-threaded runtime. Blocking work (PTY reads, `git`,
  file polling) goes on `std::thread` or `spawn_blocking`, never on a runtime
  worker.
- **Do not git commit** unless the orchestrator asks.

### Suggested ownership

| Area | Owner |
|---|---|
| `crates/civet-transport` | transport implementer |
| `crates/civetd/src/bootstrap.rs` (client side) and `src/bin/civet-cli.rs` | transport implementer |
| `crates/civet-term` | term implementer |
| `crates/civet-session` | session implementer |
| `crates/civetd/src/{main.rs,daemon/*}` (daemon wiring) | session implementer, after transport's API compiles (stub against the signatures below until then) |
| `crates/civet-desktop` (Rust + `ui/`) | frontend implementer |
| `crates/civet-proto` | frozen; changes need the orchestrator's OK |

---

## 1. civet-proto (done)

Source: `crates/civet-proto/src/*.rs`. Read the rustdoc; this section is a
map. Everything is re-exported from the crate root.

### Encoding

- Every frame on every stream: `u32` big-endian payload length, then a
  postcard payload (`MAX_FRAME_LEN` = 16 MiB).
- `encode` / `decode` (payload only), `encode_frame` (with length prefix),
  `FrameDecoder` (push bytes, pull frames; use in `select!` loops), and with the
  default `tokio` feature: `read_frame` / `write_frame` / `read_raw_frame` /
  `write_raw_frame`. `read_frame` is **not cancel-safe**: use it in a dedicated
  reader task. `read_raw_frame_limited(r, max)` caps the payload length
  (claimed lengths above it fail with `FrameTooLarge` straight from the
  header); frames read before authentication use `MAX_STREAM_OPEN_LEN`
  (64 B) for `StreamOpen` and `MAX_HELLO_LEN` (64 KiB) for the hello. Read
  buffers grow as bytes arrive; the claimed length is never pre-allocated.
- Postcard is positional: within protocol v1, enum variants may only be
  appended; struct fields may not change. No `#[serde(tag/untagged/flatten/
  skip_serializing_if)]` in proto types.
- JSON form (for the desktop UI) is serde's default external tagging with
  camelCase names: unit variants are strings (`"listPanes"`), others are
  one-key objects (`{"agentStatus": {...}}`); struct fields are camelCase,
  including the fields of struct variants (`rename_all_fields` on `Request`,
  `Response`, `Event`, `StreamOpen`, `ServerMessage`, e.g.
  `{"paneExited": {"pane": 3, "exitCode": 1}}`); postcard is unaffected;
  `SessionKey` / `CertFingerprint` / `ResumeToken` / `ClientId` are lowercase
  hex strings; `Vec<u8>` payload fields serialize as JSON arrays of numbers
  (avoid sending those over JSON; see the desktop binary channel).
  `BootstrapInfo` is the exception: snake_case field names.

### Types by purpose

| Purpose | Types |
|---|---|
| Bootstrap JSON line | `BootstrapInfo { civet_bootstrap, daemon_version, proto_version, udp_port, session_key, cert_fingerprint, pid, reattached, available_version?, live_panes?, upgrade_available }` (the last three are set only in `civetd bootstrap` output; null/false otherwise), `to_json_line`, `find_in_output` (skips MOTD noise, takes the last valid line), `BOOTSTRAP_FORMAT` |
| Secrets/ids | `SessionKey` (32 B, `ct_eq`), `CertFingerprint` (SHA-256 of cert DER), `ResumeToken` (32 B, `ct_eq`), `ClientId` (16 B, per installation), `PaneId(u64)`, `RepoId(String)`, `WorktreeId(String)` (= canonical path), `RequestId = u64` |
| Handshake | `ClientHello { magic, proto_version, session_key, client_id, client_name, client_version, resume: Option<ResumeToken> }`, `ClientHello::validate(&key) -> Result<(), HelloReject>`, `hello::peek_hello_version(&payload)`, `HelloReply::{Accepted(ServerHello), Rejected(HelloReject)}`, `ServerHello { proto_version, daemon_version, instance_id, resumed, server_time_ms, resume_token, resumed_subscriptions: Vec<SubscribeInfo> }`, `RejectReason::{BadKey, VersionMismatch, BadMagic, ShuttingDown}` |
| Stream header | `StreamOpen::{Control, Screen { pane }, Scrollback, Ports, Forward { port: u16 }}`: first frame on every QUIC stream (and on the stdio link: `Control`) |
| Ports (`ports.rs`) | `ListeningPort { port, addrs: Vec<String>, pid, process, command, pane, worktree }`, `PortsUpdate { ports }` (sorted by port), `ForwardReply = Result<(), RpcError>`, `MAX_PORT_COMMAND_CHARS` (200) |
| Envelopes | `ClientMessage::{Request(ControlRequest), Input(InputMessage), Ack(ScreenAck), Goodbye}`; `ServerMessage::{Response(ControlResponse), Event(Event), Screen(ScreenUpdate), InputAck { pane, seq }, Goodbye { reason }}` |
| RPC | `ControlRequest { id, request: Request }`, `ControlResponse { id, result: Result<Response, RpcError> }`, `RpcError { code: ErrorCode, message }` |
| Requests → responses | `Ping{nonce}`→`Pong{nonce, server_time_ms}`; `GetDaemonInfo`→`DaemonInfo`; `ListRepos`→`Repos` (git repositories only); `AddRepo{path}`→`Repo`; `RemoveRepo{repo}`→`Ok`; `ListWorktrees{repo}`→`Worktrees`; `CreateWorktree{repo, branch, base, path}`→`Worktree`; `RemoveWorktree{worktree, force}`→`Ok`; `SpawnPane(SpawnPaneRequest)`→`PaneSpawned(PaneInfo)`; `LaunchAgent(LaunchAgentRequest)`→`AgentLaunched{worktree, pane}`; `ResizePane{pane, size}`→`Ok`; `KillPane{pane, signal}`→`Ok`; `ListPanes`→`Panes`; `SubscribePane{pane, known_state}`→`Subscribed(SubscribeInfo)`; `UnsubscribePane{pane}`→`Ok`; `FetchScrollback(ScrollbackRequest)`→`Scrollback(ScrollbackPage)`; `SetAgentStatus{pane, status}`→`Ok`; `GitStatus{worktree}`→`GitStatus(GitStatusInfo)`; `GitDiff{worktree, path, old_path, staged, context_lines}`→`GitDiff(GitDiffInfo)` |
| Files (appended in 0.1.10; clients gate on `LIST_FILES_MIN_DAEMON_VERSION`) | `ListFiles{worktree, dir, include_ignored}`→`Files(DirListing { entries: Vec<FileEntry>, truncated })`, `FileEntry { name, kind: FileKind::{File, Dir, Symlink}, size, ignored }` (one directory level, `lstat`, directories first then by name, at most `MAX_DIR_ENTRIES` = 10 000; gitignored entries left out unless `include_ignored`, `.git` never); `ReadFile{worktree, path, max_bytes}`→`FilePreview { size, binary, truncated, text }` (at most `MAX_FILE_PREVIEW_BYTES` = 1 MiB; binary = NUL in the first 8 KiB, `text` then empty; regular files only); `ListAllFiles{worktree}`→`AllFiles(FileList { paths, truncated })` (`git ls-files -co --exclude-standard`, sorted, at most `MAX_ALL_FILES` = 50 000). `dir` / `path` are relative to the worktree root (`""` = root); absolute paths, `..` components and anything whose canonical path (symlinks resolved) leaves the worktree are `InvalidArgument` |
| Spawn | `SpawnPaneRequest { worktree, cwd, command: Option<CommandSpec>, env: Vec<(String,String)>, size: TermSize, title, agent }`, `CommandSpec { program, args }`, `LaunchAgentRequest { repo, branch, base, worktree_path, command, env, size, title, agent }`, `KillSignal::{Hangup (default), Terminate, Kill}` |
| Git changes (right panel → Changes) | `GitStatusInfo { staged, unstaged, untracked, conflicted: Vec<GitFileChange>, truncated }` (each list sorted by path; a path may be both staged and unstaged; at most `MAX_GIT_STATUS_ENTRIES` = 5000 entries in all), `GitFileChange { path, old_path, kind: GitChangeKind, additions: Option<u64>, deletions: Option<u64> }` (paths relative to the worktree root; counts None for binary files and for untracked files past the first 500 or over 1 MiB), `GitChangeKind::{Modified, Added, Deleted, Renamed, Copied, TypeChanged, Untracked, Conflicted}`, `GitDiffInfo { path, binary, too_large, additions, deletions, diff }` (`diff` = `git diff` output for the file, empty when `binary` / `too_large`; `too_large` past `MAX_GIT_DIFF_LINES` = 20 000 changed lines or `MAX_GIT_DIFF_BYTES` = 1 MiB). The daemon runs `git --no-optional-locks status --porcelain=v2 -z --untracked-files=all` plus `git diff [--cached -M] --numstat -z`; `GitDiff` diffs index vs. HEAD when `staged`, else working tree vs. index, and a path missing from the index against /dev/null (`--no-index`); `path` / `old_path` must stay inside the worktree (`invalidArgument`). Both appended in 0.1.11 within protocol 1, after the file requests: clients check `GIT_STATUS_MIN_DAEMON_VERSION` first |
| Git actions (right panel → Changes; appended in 0.1.14 within protocol 1, clients check `GIT_ACTIONS_MIN_DAEMON_VERSION` first) | `GitBranch{worktree}`→`GitBranch(GitBranchInfo { branch, head, upstream, ahead, behind, can_undo_commit, stashes, has_remote })` (`upstream` = the remote-tracking branch, None until it exists; `can_undo_commit` = HEAD has a parent and no remote branch contains it; `stashes` counts only entries made on this branch, since the stash list is shared by all worktrees). `GitAction{worktree, action: GitAction}`→`Ok`, or `GitDiscarded(GitDiscardSnapshot)` for a discard. `GitAction::{Stage{paths}, Unstage{paths}, Discard{paths, staged}, RestoreDiscarded(GitDiscardSnapshot), Commit{message, amend, all}, AcceptConflict{paths, side: ConflictSide::{Ours, Theirs}}, Push, Pull, Fetch, UndoCommit{head}, StashAll, StashPop}`. Paths are worktree-relative, never through a symlink out of it and never inside `.git` (`invalidArgument`); git runs with `--literal-pathspecs` and the paths on stdin, retrying briefly while another git holds `index.lock`. `Discard` first writes every working-tree file to the object database (`git hash-object -w`) and returns `GitDiscardSnapshot { entries: Vec<GitDiscardEntry { path, file: Option<GitBlobRef { oid, mode }>, restore_index, index, untracked }> }`; `RestoreDiscarded` puts index entries and files back. Without `staged` the working tree goes back to the index, with it index and working tree go back to HEAD; untracked files are deleted; unmerged paths are refused. `Push` without an upstream pushes to `origin` (or the only remote) and sets it (`-u`); `Pull` merges unless `pull.rebase` / `pull.ff` say otherwise; network actions time out after 5 min. `UndoCommit` is a compare-and-swap of HEAD (`update-ref HEAD <head>^ <head>`), refused once pushed. `StashAll` = `stash push -u`; `StashPop` applies this branch's newest entry, then drops it by id. git's refusals are `git` errors with its stdout + stderr |
| File ops (right panel → Files; appended in 0.1.15 within protocol 1, after the git actions: clients check `FILE_OPS_MIN_DAEMON_VERSION` first) | `FileOp{worktree, op}`→`FileOpDone(FileOpResult { path, trash_id })` with `op` one of `FileOp::{CreateFile{path}, CreateDir{path}, Move{from, to}, Copy{from, to}, Trash{path}, Restore{trash_id}, Write{path, data}}`; `ReadFileChunk{worktree, path, offset, len}`→`FileChunk { size, data }` (at most `MAX_FILE_CHUNK_BYTES` = 8 MiB per call, regular files only; for downloads). Paths follow the `ListFiles` rules, and none may name `.git` or anything in it (also through a symlink); the root itself is never a source. Nothing is overwritten: an existing target is `AlreadyExists` (renames use `renameat2(RENAME_NOREPLACE)` / `renamex_np(RENAME_EXCL)`; a case-only rename of the same file is allowed). `Move` / `Trash` act on a symlink itself and refuse a directory with its own `.git` (nested repo, submodule); `Copy` recreates symlinks as links and leaves `.git` entries out; across file systems `Move` / `Trash` / `Restore` copy everything (`.git` included; a fifo, socket or device fails the move before anything is deleted) and keep the copy if removing the source fails; a directory into its own subtree is `InvalidArgument`. `Write` (at most `MAX_UPLOAD_BYTES`) creates a new file. `Trash` moves the entry to `<state>/trash/<id>/item` with `meta.json` (worktree, path, time) and returns the id; `Restore` puts it back at its old path in the same worktree (`NotFound` for an unknown id or another worktree). Entries older than 3 days, and all but the newest 200, are purged whenever something is trashed |
| Projects (folder projects; appended in 0.1.17 within protocol 1, after the file ops: clients check `PROJECTS_MIN_DAEMON_VERSION` first and fall back to `ListRepos` / `AddRepo`) | `ListProjects`→`Projects(Vec<ProjectInfo { repo: RepoInfo, kind }>)`, every project in the order added; `AddProject{path}`→`Project(ProjectInfo)`: the git repository containing `path` (as `AddRepo`), else the directory itself as `kind: ProjectKind::Folder` (RepoId: the same hash of its canonical path, `default_branch` None). A folder has one worktree, its root (`is_main`, `branch` and `head` None), for `ListFiles` (nothing ignored), `ListAllFiles` (a walk of every file, `.git` left out, symlinks not followed), `ReadFile`, `ReadFileChunk` and `FileOp`. `GitStatus`, `GitDiff`, `GitBranch`, `GitAction`, `ListPullRequests`, `CreateWorktree` and `LaunchAgent` with that worktree are `Unsupported`; `SpawnPane` in it works (older daemons refuse it as `Unsupported`). Once the folder holds a `.git` of its own (`git init`), `ListProjects` / `ListRepos` re-register it as a repository in place (a new RepoId, the same worktree id) and emit `ReposChanged`; the worktree watcher emits `ReposChanged` when such a `.git` appears, so clients re-list. `ListRepos` leaves folders out so older clients never see one; `AddRepo` still refuses a folder (`InvalidArgument`, "… is not a git repository") |
| Folder completion (Add project → Folder; appended in 0.1.18 within protocol 1, after the projects: clients check `LIST_DIRS_MIN_DAEMON_VERSION` first) | `ListDirs{path}`→`Dirs(HostDirListing { path, dirs: Vec<HostDir { name, git_repo }>, truncated })`: the subdirectories of any directory on the host (absolute, `~` / `~/…`, anything else relative to the home directory; `path` is the expanded directory), hidden ones included, symlinks to directories followed, sorted by name (case-insensitive), at most `MAX_DIR_ENTRIES`. `git_repo` = has `.git` (repository or worktree root) or is a bare repository (`HEAD`, `objects/`, `refs/`); stats only, no git. A missing directory is `NotFound`, a file `InvalidArgument` |
| Host stats (stats view; appended in 0.2.1 within protocol 1, after `ListDirs`: clients check `HOST_STATS_MIN_DAEMON_VERSION` first) | `HostStats{since, processes}`→`HostStats(HostStats { interval_ms, samples: Vec<StatsSample { seq, at_ms, cpu, mem_used, swap_used, net_rx, net_tx, disk_read, disk_write, gpus }>, cores, mem_total, swap_total, load, uptime_secs, disks, gpus, processes })`. Polled on the control stream, so it works on stream links too. The daemon samples every 2 s (sysinfo; GPUs via `nvidia-smi`) while anyone polls and stops a minute after the last request, keeping up to `MAX_STATS_SAMPLES` (10 min). `since` = newest seq the client has; a `since` newer than the newest sample (another daemon run) returns everything. CPU / GPU busy in permille, rates in bytes per second. With `processes` (sampled only while asked for in the last 10 s): every process in a pane's tree with its pane and worktree, then the busiest others, at most `MAX_STATS_PROCESSES` |
| Worktree lifecycle (sidebar ahead / behind, Clean up worktrees, Update from main; appended in 0.4.1 within protocol 1, after `HostStats`: clients check `WORKTREE_LIFECYCLE_MIN_DAEMON_VERSION` first) | `WorktreeLifecycle{repo, fetch, review}`→`WorktreeLifecycle(RepoLifecycle { base, worktrees: Vec<WorktreeLifecycle { worktree, branch, ahead, behind, upstream, upstream_ahead, upstream_behind, upstream_gone, unpushed, dirty, pull_request, stale: Vec<StaleReason> }>, fetch_error, pull_requests_error })`. `base` = `origin/<default>` if that remote-tracking branch exists, else `<default>`; `ahead` / `behind` are against it, from the commit `git worktree list` reports (run in the main worktree, read only). `unpushed` = commits on neither the base nor any remote-tracking branch. With `fetch`, `git fetch --prune --all` first (failure → `fetch_error`, counts from the last fetch). With `review`, `dirty` = paths in `git status` (untracked included) and `pull_request` = the branch's newest PR from `gh` (failure → `pull_requests_error`). `StaleReason::{Merged, UpstreamGone, PullRequestMerged, PullRequestClosed}`: `Merged` = the tip is in the base's history but not on its first-parent line (a fresh branch is not merged); `UpstreamGone` = commits of its own and an upstream configured but missing, only once the branch is known to have been pushed (it has a PR, or this daemon saw the upstream exist: Civet configures a new branch's upstream before its first push); the PR reasons only while HEAD is the PR's head or behind it (a reused branch name is not stale). Never stale: the main worktree, a detached HEAD, the default branch. `UpdateWorktree{worktree, strategy: UpdateStrategy::{Rebase, Merge}}`→`WorktreeUpdated(WorktreeUpdate { base, outcome: UpdateOutcome::{UpToDate, Updated, MergeConflicts, RebaseConflicts}, conflicts })`: fetches `origin <default>` (when `origin/<default>` exists), then `git rebase` / `git merge --no-edit` the base. A conflicting rebase is aborted (nothing changes); a conflicting merge is left for the Changes tab. Refused (`invalidArgument`) on a detached HEAD, the default branch, with uncommitted changes to tracked files or a merge / rebase in progress; a failed fetch is a `git` error. Emits `WorktreesChanged` unless up to date |
| Info | `RepoInfo`, `ProjectInfo { repo, kind: Git \| Folder }`, `WorktreeInfo`, `PaneInfo { id, worktree, cwd, command, title, size, agent, status, exit_code, exited, created_at_ms, state }`, `DaemonInfo`, `SubscribeInfo { pane, state, size, history_lines, last_input_seq }` |
| Events (pushed to every client) | `Event::{PaneCreated(PaneInfo), PaneExited{pane, exit_code}, PaneClosed{pane}, PaneTitle{pane, title}, PaneResized{pane, size}, AgentStatus(AgentStatusEvent), Bell{pane}, ReposChanged, WorktreesChanged{repo}, Clipboard{pane, text}}`, `Event::pane()`. `Clipboard` (OSC 52 set, appended in 0.1.1 within protocol 1) is the exception: only sent to clients subscribed to the pane whose `client_version` is at least `CLIPBOARD_EVENT_MIN_CLIENT_VERSION` (older clients cannot decode it), at most `MAX_CLIPBOARD_BYTES` (1 MiB), never replayed on resume / Full |
| Terminal | `TermSize { cols, rows, pixel_width, pixel_height }` (`sanitized()` clamps to 1x1..=`MAX_TERM_COLS` x `MAX_TERM_ROWS` = 1000 x 500), `InputMessage { pane, seq, data }`, `ScreenUpdate { pane, state, base_state, kind: UpdateKind::{Full, Diff}, acked_input_seq, size, history_lines, history_start, payload }`, `ScreenAck { pane, state }` |
| Scrollback | `ScrollbackRequest { req_id, pane, query }`, `ScrollbackQuery::{Before { end: Option<u64>, limit }, Range { start, end }}`, `ScrollbackPage { req_id, pane, start, lines: Vec<ScrollbackLine>, oldest_available, end_available }`, `ScrollbackLine { vt, wrapped }` |
| Agent status | `AgentStatus::{Unknown (default), Running, NeedsInput, Done, Idle}` (`FromStr` is lenient), `AgentStatusSource::{EscapeCode, Hook, Heuristic, ProcessExit, Client}`, `AgentStatusEvent { pane, status, previous, source, message, at_ms }`, `parse_status_file(&str) -> Option<(AgentStatus, Option<String>)>` |
| Constants | `PROTOCOL_VERSION = 1`, `MIN_PROTOCOL_VERSION`, `MAGIC = b"CIVT"`, `ALPN = b"civet/1"`, `MAX_INFLIGHT_UPDATES = 8`, `MAX_REPLAY_LINES = 2000`, `FULL_REPLAY_LINES = 1000`, `ECHO_ACK_DELAY_MS = 50`, `ENV_PANE_ID`, `ENV_STATUS_FILE`, `ENV_STATE_DIR`, `STATE_DIR_NAME`, `MAX_CLIPBOARD_BYTES` (1 MiB), `CLIPBOARD_EVENT_MIN_CLIENT_VERSION` ("0.1.1"), `UPLOAD_FILE_MIN_DAEMON_VERSION` ("0.1.4"), `LIST_PULL_REQUESTS_MIN_DAEMON_VERSION` ("0.1.5"), `LIST_FILES_MIN_DAEMON_VERSION` ("0.1.10"), `MAX_DIR_ENTRIES`, `MAX_FILE_PREVIEW_BYTES`, `MAX_ALL_FILES`, `now_ms()`, `is_compatible(v)` |

### Stream layout (normative)

| Stream | Opened by | Frames |
|---|---|---|
| Control (bidi, exactly one, the first) | client | C→S: `StreamOpen::Control`, `ClientHello`, then `ClientMessage`*. S→C: `HelloReply`, then `ServerMessage`* (`Response`, `Event`, `InputAck`, `Goodbye`; on QUIC never `Screen`) |
| Screen (uni, one per subscribed pane per connection) | daemon | `StreamOpen::Screen { pane }`, then `ServerMessage::Screen`* for that pane in state order. Daemon `finish()`es it on unsubscribe or pane close |
| Scrollback (bidi, one per request) | client | C→S: `StreamOpen::Scrollback`, `ScrollbackRequest`, FIN. S→C: `Result<ScrollbackPage, RpcError>`, FIN |
| Ports (bidi, at most one per connection) | client | C→S: `StreamOpen::Ports`, then nothing (the client closes it to stop). S→C: one `PortsUpdate` at once, another whenever the daemon user's listening TCP ports change (scanned every 2 s while anyone watches) |
| Forward (bidi, one per forwarded TCP connection) | client | C→S: `StreamOpen::Forward { port }`. S→C: one `ForwardReply` (`Err(NotFound)`: nothing listens on `127.0.0.1` / `::1` at `port`). After `Ok`: raw TCP bytes both ways, FIN = half-close. Send priority −1 (below control and screen streams) |
| **Stdio link** (SSH fallback, unix socket): one byte stream | client | Same as Control, except screen updates arrive as `ServerMessage::Screen` on it and scrollback uses `Request::FetchScrollback` → `Response::Scrollback` |

Rules:

- The daemon closes the connection (QUIC application close code 1, or EOF on
  a stdio link) right after sending `HelloReply::Rejected`.
- Clients may pipeline requests right after the `ClientHello` without waiting
  for the reply. If the hello is rejected they are discarded.
- Responses may arrive out of order relative to requests (match on `id`).
  Events and responses share the control stream in send order.
- A screen update for a pane may arrive before the `Subscribed` response.
- `Ports` and `Forward` were added within protocol 1 without a version
  gate: they never touch the control stream, and a daemon that predates them
  fails to decode the header and drops only that stream (the client reports
  `ErrorCode::Unsupported`). They do not exist on the stdio link.

---

## 2. civet-transport

Moves civet-proto frames between client and daemon over QUIC (quinn), or over
any byte stream (SSH stdio fallback, unix socket, tests), behind one API so
higher layers do not care which. Also owns the client-side **resumable
session** (reconnect, resume token, input retransmission, re-subscription,
stall detection).

Module layout (suggested): `cert.rs`, `quic/{server,client}.rs`,
`stream.rs` (stdio link), `conn.rs` (shared `ClientConnection` /
`ServerConnection`), `session.rs` (`ClientSession`), `error.rs`.

### 2.1 Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Proto(#[from] civet_proto::ProtoError),
    /// quinn connect/connection/write/read errors, stringified so quinn types
    /// don't leak into the public API.
    #[error("quic: {0}")]
    Quic(String),
    #[error("tls/certificate: {0}")]
    Tls(String),
    #[error("certificate fingerprint mismatch: expected {expected}, got {actual}")]
    FingerprintMismatch { expected: CertFingerprint, actual: CertFingerprint },
    /// Daemon refused the hello. `BadKey` => daemon restarted: re-bootstrap.
    #[error("hello rejected: {0:?}")]
    Rejected(civet_proto::HelloReject),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("peer violated protocol: {0}")]
    ProtocolViolation(String),
    /// Connection closed (cleanly or not). For ClientConnection: the link is
    /// gone; ClientSession reconnects on its own.
    #[error("connection closed: {0}")]
    Closed(String),
    /// The daemon closed the connection with code 3: a newer connection with
    /// the same ClientId took over.
    #[error("connection taken over by another connection of this client: {0}")]
    Replaced(String),
}

impl TransportError {
    /// True if retrying the same dial cannot help (Rejected with
    /// BadKey/VersionMismatch/BadMagic, FingerprintMismatch).
    pub fn is_fatal(&self) -> bool;
}
```

### 2.2 Certificates and pinning (`cert.rs`)

```rust
/// Daemon TLS identity: self-signed ECDSA P-256 cert (rcgen), subject alt
/// name "civetd", validity 10 years.
pub struct ServerIdentity { /* CertificateDer<'static>, PrivateKeyDer<'static> */ }

impl ServerIdentity {
    pub fn generate() -> Result<Self, TransportError>;
    /// Load `<dir>/cert.der` + `<dir>/key.der`, or generate and write them
    /// (key file mode 0600, written via temp file + rename). Keeps the
    /// fingerprint stable across daemon restarts.
    pub fn load_or_generate(dir: &std::path::Path) -> Result<Self, TransportError>;
    pub fn fingerprint(&self) -> CertFingerprint;
    pub fn cert_der(&self) -> &[u8];
}

/// SHA-256 over the DER bytes of a certificate.
pub fn fingerprint_of(cert_der: &[u8]) -> CertFingerprint;
```

Client verification: a custom `rustls::client::danger::ServerCertVerifier`
that accepts the end-entity certificate **iff** `fingerprint_of(cert) ==
expected` (constant-time compare), ignores the server name and the chain, and
verifies handshake signatures with the ring provider's
`verify_tls12_signature` / `verify_tls13_signature` (do **not** skip signature
verification). TLS 1.3 only, ALPN `civet_proto::ALPN`. Use quinn's
`QuicClientConfig::try_from(rustls::ClientConfig)`. Server name for SNI:
`"civetd"`. On mismatch, surface `TransportError::FingerprintMismatch`
(capture the actual fingerprint in the verifier).

### 2.3 QUIC parameters (both sides unless noted)

| Setting | Value | Why |
|---|---|---|
| `max_idle_timeout` | 6 h | A laptop that wakes within 6 h continues the *same* connection (migration, zero handshakes). |
| `keep_alive_interval` (client only) | 2 s | Keeps NAT bindings while awake; nothing is sent while asleep. |
| Server `migration` | `true` (quinn default) | Client IP/port changes follow automatically. |
| `initial_rtt` | 100 ms | |
| Stream limits | bidi 512, uni 1024 | One uni stream per subscribed pane; bidi: scrollback fetches and one per forwarded TCP connection. |
| Connect (handshake + hello) timeout | 5 s (client config) | |
| Daemon hello read timeout | 10 s | Then drop the connection. |

Errors mapping quinn close reasons into `TransportError::Closed`
(`Replaced` for code 3). Application close codes: 0 = normal (`Goodbye`),
1 = hello rejected, 2 = protocol violation, 3 = replaced by a newer
connection of the same client, 4 = daemon shutting down. The daemon keeps
one live connection per `ClientId`, so every client app needs its own id
(desktop: `client-id-desktop`; civet-cli: random per run unless
`--client-id`).

Stateless resets: the daemon's endpoint uses a reset key derived from the
persisted TLS identity key (HMAC-SHA256 with a fixed label; never the
per-start session key), so a restarted daemon's stateless resets are
recognized by clients of the previous instance and they notice the dead
connection at once instead of after a timeout.

### 2.4 Server side

```rust
pub struct QuicServerConfig {
    /// e.g. 0.0.0.0:0. Use `bind_port_range` to choose the port.
    pub bind: std::net::SocketAddr,
    pub identity: ServerIdentity,
    pub session_key: SessionKey,
}

/// UDP/QUIC listener. Clone-free; share via Arc.
pub struct QuicServer { /* quinn::Endpoint */ }

impl QuicServer {
    pub fn bind(config: QuicServerConfig) -> Result<Self, TransportError>;
    /// Try ports in `range` in order (mosh-style 60001..=60999) on `ip`; first
    /// that binds wins.
    pub fn bind_port_range(
        ip: std::net::IpAddr,
        range: std::ops::RangeInclusive<u16>,
        identity: ServerIdentity,
        session_key: SessionKey,
    ) -> Result<Self, TransportError>;
    pub fn local_addr(&self) -> std::net::SocketAddr;
    pub fn fingerprint(&self) -> CertFingerprint;
    /// Next incoming connection; None once `close()` was called. Returns
    /// quickly: do the handshake in a spawned task.
    pub async fn accept(&self) -> Option<IncomingConnection>;
    /// Close all connections with code 4 and stop accepting.
    pub fn close(&self, reason: &str);
}

pub struct IncomingConnection { /* quinn::Incoming + key */ }

impl IncomingConnection {
    pub fn remote_addr(&self) -> std::net::SocketAddr;
    /// TLS handshake, accept the control stream, read `StreamOpen::Control` +
    /// `ClientHello` (10 s timeout), and validate magic / version / session
    /// key with `ClientHello::validate`. On failure it sends
    /// `HelloReply::Rejected` itself, closes, and returns `Err(Rejected)`.
    /// On success the hello is NOT yet answered: the daemon decides about
    /// resume and calls `ServerConnection::accept`. Pre-auth frames are
    /// size-limited (`MAX_STREAM_OPEN_LEN`, `MAX_HELLO_LEN`).
    pub async fn handshake(self) -> Result<ServerConnection, TransportError>;
}

/// Same as `IncomingConnection::handshake` for an already-connected byte
/// stream (unix socket from `civetd stdio-tunnel`, or a test duplex).
pub async fn accept_stream<S>(io: S, session_key: &SessionKey)
    -> Result<ServerConnection, TransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind { Quic, Stream }

/// An authenticated connection whose hello is not yet answered.
pub struct ServerConnection { /* enum Link { Quic(..), Stream(..) } */ }

impl ServerConnection {
    pub fn hello(&self) -> &ClientHello;
    pub fn kind(&self) -> TransportKind;
    /// None for stream links.
    pub fn remote_addr(&self) -> Option<std::net::SocketAddr>;
    /// Send `HelloReply::Accepted(hello)` and start the I/O tasks.
    pub async fn accept(self, hello: ServerHello)
        -> Result<(ServerSender, ServerReceiver), TransportError>;
    /// Send `HelloReply::Rejected` and close (code 1).
    pub async fn reject(self, reject: HelloReject);
}

/// Cheap to clone; all methods are usable from any task.
#[derive(Clone)]
pub struct ServerSender { /* .. */ }

impl ServerSender {
    pub fn kind(&self) -> TransportKind;
    /// Queue a message on the control stream (Response, Event, InputAck,
    /// Goodbye). Messages are written in call order. Returns when the frame
    /// is accepted by the stream (backpressure), not when delivered.
    /// Passing `ServerMessage::Screen` here is allowed but bypasses per-pane
    /// streams; use `open_screen_stream`.
    pub async fn send(&self, msg: ServerMessage) -> Result<(), TransportError>;
    /// Per-pane screen channel. QUIC: opens a uni stream and writes
    /// `StreamOpen::Screen { pane }`. Stream link: a handle that writes
    /// `ServerMessage::Screen` frames into the shared stream.
    pub async fn open_screen_stream(&self, pane: PaneId)
        -> Result<ScreenStream, TransportError>;
    /// Close the connection with an application code (2/3/4, see 2.3).
    pub fn close(&self, code: u32, reason: &str);
    /// Resolves when the connection is gone (either side, any reason).
    pub async fn closed(&self) -> TransportError;
    /// Current RTT estimate (QUIC), None for stream links.
    pub fn rtt(&self) -> Option<std::time::Duration>;
    /// Current client address (changes on migration), None for streams.
    pub fn remote_addr(&self) -> Option<std::net::SocketAddr>;
}

pub struct ScreenStream { /* .. */ }

impl ScreenStream {
    pub fn pane(&self) -> PaneId;
    /// Write one `ServerMessage::Screen(update)` frame. Awaits flow-control
    /// credit: callers rely on this to coalesce (see civet-session).
    pub async fn send(&mut self, update: &ScreenUpdate) -> Result<(), TransportError>;
    /// Finish the stream (QUIC FIN); no-op on stream links.
    pub async fn finish(self);
}

pub struct ServerReceiver { /* .. */ }

pub enum ServerIncoming {
    /// Anything on the control stream except `Request::FetchScrollback`.
    Message(ClientMessage),
    /// From a Scrollback stream (QUIC) or a `Request::FetchScrollback` on the
    /// control stream (normalized here so the daemon has one code path).
    Scrollback { request: ScrollbackRequest, responder: ScrollbackResponder },
}

impl ServerReceiver {
    /// Next incoming item. Ok(None) after a clean close / Goodbye.
    /// Cancel-safe (backed by an mpsc fed by reader tasks).
    pub async fn recv(&mut self) -> Result<Option<ServerIncoming>, TransportError>;
}

pub struct ScrollbackResponder { /* stream or (control id, sender) */ }

impl ScrollbackResponder {
    /// QUIC: write the result on the scrollback stream and finish it.
    /// Stream link: send `ControlResponse { id, result: .map(Response::Scrollback) }`.
    pub async fn respond(self, result: Result<ScrollbackPage, RpcError>)
        -> Result<(), TransportError>;
}
```

### 2.5 Client side: one connection

```rust
#[derive(Debug, Clone)]
pub struct QuicClientConfig {
    /// Resolved daemon address (host from `ssh -G`, port from bootstrap).
    pub addr: std::net::SocketAddr,
    pub fingerprint: CertFingerprint,
    pub connect_timeout: std::time::Duration, // default 5 s
}

/// Owns the client UDP endpoint. One per host is fine; share via Arc.
pub struct QuicClient { /* quinn::Endpoint */ }

impl QuicClient {
    /// Bind an endpoint on an unspecified address of the given family
    /// (0.0.0.0:0 or [::]:0).
    pub fn new(ipv6: bool) -> Result<Self, TransportError>;
    /// Replace the endpoint's UDP socket with a freshly bound one
    /// (`quinn::Endpoint::rebind(std::net::UdpSocket::bind(..)?)`). Every
    /// connection on the endpoint migrates to the new socket; the server
    /// follows on the next packet. Call after wake / network change.
    pub fn rebind(&self) -> std::io::Result<()>;
    /// Dial, pin the cert, open the control stream, send
    /// `StreamOpen::Control` + `hello`, flush, then send each of `pipelined`
    /// (e.g. SubscribePane requests) **before** awaiting the HelloReply.
    pub async fn connect(
        &self,
        config: &QuicClientConfig,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> Result<Connected, TransportError>;
}

/// Same over an arbitrary byte stream (SSH stdio tunnel, unix socket, test duplex).
pub async fn connect_stream<S>(
    io: S,
    hello: ClientHello,
    pipelined: Vec<ClientMessage>,
) -> Result<Connected, TransportError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static;

#[derive(Debug)]
pub struct Connected {
    pub conn: ClientConnection,
    pub incoming: ClientReceiver,
    pub server_hello: ServerHello,
}

/// Transport-agnostic client connection handle. Clone = same connection.
#[derive(Clone)]
pub struct ClientConnection { /* Arc<enum Link { Quic(..), Stream(..) }> */ }

impl ClientConnection {
    pub fn kind(&self) -> TransportKind;
    /// Queue a message on the control stream, in call order.
    pub async fn send(&self, msg: ClientMessage) -> Result<(), TransportError>;
    /// QUIC: open a Scrollback bidi stream. Stream link: send
    /// `Request::FetchScrollback` and wait for the matching response.
    pub async fn fetch_scrollback(&self, req: ScrollbackRequest)
        -> Result<Result<ScrollbackPage, RpcError>, TransportError>;
    /// Delegates to `QuicClient::rebind`; Ok(()) no-op for stream links.
    pub fn rebind(&self) -> std::io::Result<()>;
    pub fn rtt(&self) -> Option<std::time::Duration>;
    /// Send `ClientMessage::Goodbye`, then close with code 0.
    pub async fn close(&self);
    pub async fn closed(&self) -> TransportError;
}

pub struct ClientReceiver { /* mpsc fed by control + screen stream readers */ }

impl ClientReceiver {
    /// Next `ServerMessage` from the control stream or any screen stream
    /// (screen streams are read as they are opened by the daemon). Per pane,
    /// screen updates are delivered in order. Ok(None) on clean close.
    /// Cancel-safe. Scrollback responses on stream links are routed to
    /// `fetch_scrollback` callers, not returned here.
    pub async fn recv(&mut self) -> Result<Option<ServerMessage>, TransportError>;
}
```

The "common Connection" abstraction is these four types
(`ServerConnection`/`ServerSender`/`ServerReceiver` and
`ClientConnection`/`ClientReceiver`): each wraps a private
`enum Link { Quic(QuicLink), Stream(StreamLink) }`. Nothing above the
transport matches on the link type except via `kind()` for display.

### 2.6 Client side: resumable session (`session.rs`)

`ClientSession` is what civet-cli and civet-desktop use. It owns the current
`ClientConnection`, reconnects on its own, and hides migrations and resumes
from the caller.

```rust
/// How to (re)open a link. Implemented for QUIC and for stream links.
pub trait Dialer: Send + Sync + 'static {
    fn dial(
        &self,
        hello: ClientHello,
        pipelined: Vec<ClientMessage>,
    ) -> futures::future::BoxFuture<'_, Result<Connected, TransportError>>;
    /// Called on network change; QUIC rebinds its endpoint. Default: Ok(()).
    fn rebind(&self) -> std::io::Result<()> { Ok(()) }
    /// Kind produced (failover dialer: the preferred kind).
    fn kind(&self) -> TransportKind;
    /// Session hint: link in trouble since `since` (stall / loss), or
    /// recovered (None). Default: ignored.
    fn link_trouble(&self, since: Option<std::time::Instant>) {}
    /// While connected over a link whose kind != `kind()` (a fallback), the
    /// session calls `dial_preferred` this often and adopts the result on
    /// success (resume). Default None.
    fn upgrade_interval(&self) -> Option<std::time::Duration> { None }
    /// Dial only the preferred link. Default: `dial`.
    fn dial_preferred(&self, hello: ClientHello, pipelined: Vec<ClientMessage>)
        -> BoxFuture<'_, Result<Connected, TransportError>>;
}

/// QUIC first, fallback (SSH stdio tunnel) when it fails; back to QUIC when
/// it works again (`failover.rs`).
pub struct FailoverDialer { /* .. */ }
impl FailoverDialer {
    pub fn new(primary: Arc<dyn Dialer>, fallback: Arc<dyn Dialer>, config: FailoverConfig) -> Self;
    pub fn on_fallback(&self) -> bool;
}
pub struct FailoverConfig {
    pub primary_timeout: Duration,         // 3 s, bound on one primary dial
    pub fail_over_after_attempts: u32,     // 2 consecutive failed primary dials
    pub fail_over_after: Duration,         // 3 s of trouble (or one 3 s attempt)
    pub probe_interval: Duration,          // 10 s, retry the primary while on the fallback
}
```

`FailoverDialer::dial`: on the fallback → fallback. Otherwise the primary;
after `fail_over_after_attempts` failures or an attempt that took
`fail_over_after`, the fallback in the same call (so a first connect with
UDP blocked also succeeds). Once the session reports trouble lasting
`fail_over_after`, it races primary and fallback (first hello wins).
Fatal errors (`is_fatal`, `Rejected`) are returned as-is.

```rust

pub struct QuicDialer { pub client: std::sync::Arc<QuicClient>, pub config: QuicClientConfig }
impl Dialer for QuicDialer { /* .. */ }

/// Dials by calling a factory that returns a fresh byte stream (e.g. spawns
/// `ssh host civetd stdio-tunnel`).
pub struct StreamDialer<F> { pub factory: F }
impl<F, Fut, S> Dialer for StreamDialer<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::io::Result<S>> + Send,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static;

#[derive(Debug, Clone)]
pub struct ClientSessionConfig {
    pub session_key: SessionKey,
    pub client_id: ClientId,
    pub client_name: String,     // "civet-desktop" | "civet-cli"
    pub client_version: String,  // env!("CARGO_PKG_VERSION")
    /// Send a Ping when nothing was received for this long. Default 1 s.
    pub heartbeat: std::time::Duration,
    /// A ping unanswered for this long => `LinkState::Recovering`. Default 1 s.
    pub stall_after: std::time::Duration,
    /// While Recovering for this long, dial a fresh connection in parallel
    /// (with the resume token) and keep whichever answers first. Default 3 s.
    pub race_after: std::time::Duration,
    /// Reconnect backoff schedule after a connection is lost; the last value
    /// repeats forever. Default [0, 100, 250, 500, 1000, 2000, 5000] ms.
    pub backoff: Vec<std::time::Duration>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkState {
    Connecting,
    Connected { transport: TransportKind, rtt_ms: Option<u32> },
    /// Something is wrong (stall, lost connection, network change) and
    /// recovery is running. `since_ms` = civet_proto::now_ms() when it began.
    /// The UI shows "reconnecting" only if this lasts > 1 s (desktop's job).
    Recovering { since_ms: u64, reason: String },
    /// Fatal: needs re-bootstrap (BadKey / instance gone / version mismatch)
    /// or the user closed it.
    Failed { reason: String, needs_bootstrap: bool },
    Closed,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Validated, in-order screen update for a subscribed pane. For a `Diff`,
    /// `base_state` equalled the pane's current client state. The session
    /// has already queued the `ScreenAck` for it.
    Screen(ScreenUpdate),
    /// Highest input seq the daemon applied for this pane (from
    /// `ServerMessage::InputAck` or `ScreenUpdate::acked_input_seq`).
    InputAcked { pane: PaneId, seq: u64 },
    Event(Event),
    Link(LinkState),
    /// A (re)connection completed. `resumed=false` after the first connect or
    /// when resume failed. `instance_changed` => daemon restarted: all panes
    /// are gone; the caller should drop its pane views.
    Connected { resumed: bool, instance_changed: bool, server_hello: ServerHello },
    /// Daemon said Goodbye (upgrade/shutdown).
    Goodbye { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// The connection died before the response arrived. Requests are not
    /// retried automatically (they may not be idempotent).
    #[error("connection lost before response")]
    ConnectionLost,
    #[error("session closed")]
    Closed,
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

/// Clone = same session.
#[derive(Clone)]
pub struct ClientSession { /* Arc<Inner> */ }

impl ClientSession {
    /// First connect: dial once (no retries) so the caller can fall back to
    /// another Dialer (QUIC -> SSH stdio) on error. Then spawns the
    /// supervisor task. Events go to the returned receiver (unbounded or a
    /// large bound; screen updates must never be dropped).
    pub async fn connect(
        config: ClientSessionConfig,
        dialer: std::sync::Arc<dyn Dialer>,
    ) -> Result<(ClientSession, tokio::sync::mpsc::UnboundedReceiver<SessionEvent>), TransportError>;

    pub fn link_state(&self) -> tokio::sync::watch::Receiver<LinkState>;
    pub fn server_hello(&self) -> ServerHello;

    /// Generic RPC; prefer the helpers below for subscribe/scrollback.
    pub async fn request(&self, request: Request) -> Result<Response, RequestError>;

    /// Subscribe (or reset to Full if already subscribed). The session
    /// remembers subscriptions and re-subscribes after every reconnect with
    /// `known_state = current client state`.
    pub async fn subscribe(&self, pane: PaneId) -> Result<SubscribeInfo, RequestError>;
    pub async fn unsubscribe(&self, pane: PaneId) -> Result<(), RequestError>;

    /// Assign the next seq for this pane, buffer the bytes until acked, and
    /// send. Never blocks on the network; returns the seq, or 0 (nothing
    /// buffered or sent) for a pane known to be closed.
    pub fn send_input(&self, pane: PaneId, data: Vec<u8>) -> u64;

    pub async fn fetch_scrollback(&self, pane: PaneId, query: ScrollbackQuery)
        -> Result<ScrollbackPage, RequestError>;

    /// Hint from the OS/UI: wake from sleep or network change. Rebinds the
    /// QUIC endpoint, sends a Ping immediately, and starts stall detection.
    /// This is what makes "the next packet is the reconnect" true.
    pub fn notify_network_change(&self);

    /// Goodbye + close; LinkState::Closed.
    pub async fn close(&self);
}
```

Required behavior:

1. **Per-pane client state** kept by the session: `current_state` (0 =
   nothing applied), `next_input_seq`, `unacked_input: VecDeque<(seq, bytes)>`,
   `subscribed: bool`.
2. **Screen validation.** `Full` is always applied. `Diff` is applied only if
   `base_state == current_state`; otherwise drop it and send
   `SubscribePane { known_state: None }` once (forces Full) until a Full
   arrives. After applying: `current_state = state`, emit
   `SessionEvent::Screen`, send `ScreenAck` (may coalesce acks per pane
   within 10 ms, always acking the newest).
3. **Input.** `unacked_input` entries with `seq <= acked` are dropped on
   `acked_input_seq` / `InputAck`. Seqs start at 1 per pane per *fresh*
   session.
4. **Reconnect.** On connection loss: `LinkState::Recovering`, then dial with
   `ClientHello.resume = Some(last resume_token)` and pipeline
   `SubscribePane { pane, known_state: Some(current_state) }` for every
   subscribed pane. On the reply:
   - `resumed = true`: for each pane in `resumed_subscriptions`, drop
     unacked input with `seq <= last_input_seq` and resend the rest in order.
   - `resumed = false`, same `instance_id`: clear all `unacked_input` (it may
     have been applied; never risk duplicating keystrokes), reset
     `next_input_seq` to 1 per pane. Subscriptions were already re-sent.
   - `instance_id` changed: daemon restarted; clear all pane state, emit
     `Connected { instance_changed: true, .. }`.
   - `Rejected(BadKey)`: `LinkState::Failed { needs_bootstrap: true }`, stop.
   - A pipelined re-subscribe answered `NotFound` (the pane closed while
     the client was away, so its `PaneClosed` was never delivered): drop the
     pane's state and emit `SessionEvent::Event(Event::PaneClosed)`; later
     `send_input` for it returns 0 without buffering.
   Always store the new `resume_token`.
   - **Replaced** (QUIC close code 3) while no dial of its own is in flight:
     another connection with this `ClientId` took over. The session redials
     once after `REPLACED_RETRY_DELAY` (1 s); a second takeover within
     `REPLACED_WINDOW` (30 s) ends it with `LinkState::Failed { reason:
     REPLACED_REASON, needs_bootstrap: false }`, so two clients sharing an
     id cannot reconnect-storm. From the second consecutive connection lost
     less than 500 ms after it was established, the reconnect backoff
     escalates instead of redialing at once (covers stream links, which
     carry no close code).
   - **Input retransmit.** Unacked input of a pane that saw no ack progress
     for `INPUT_RETRANSMIT_AFTER` (1 s) is resent on the same connection
     (at most 64 KiB per period; the daemon dedups by seq). This is how
     input refused under daemon PTY backpressure gets applied.
   - **Failover.** With a dialer whose `upgrade_interval()` is Some (the
     `FailoverDialer`), while connected over a non-preferred link the
     session periodically dials `dial_preferred` (resume token + pipelined
     re-subscribes) and adopts it on success. `LinkState::Connected.transport`
     is the kind of the current connection.
5. **Stall detection.** Track `last_rx` (any frame). If nothing was received
   for `heartbeat`, send `Ping`. If a ping is unanswered for `stall_after`,
   go `Recovering` and call `dialer.rebind()`. If still recovering after
   `race_after`, dial a second connection in parallel; first to complete the
   hello wins, the other is closed. When any frame arrives on the current
   connection, go back to `Connected` (`rtt_ms` from QUIC stats or ping RTT).
6. **Requests in flight** when a connection dies fail with
   `RequestError::ConnectionLost`, except `subscribe` (re-sent automatically).
7. The session never sleeps the thread and never drops screen events.

Tests to include: in-memory duplex (`tokio::io::duplex`) and loopback QUIC
round-trips; fingerprint mismatch is rejected; wrong session key gets
`Rejected(BadKey)`; `rebind()` mid-stream keeps the connection alive and
in-order; killing the connection triggers a resume where buffered input is
resent exactly once.

---

## 3. civet-term

Server-side terminal emulation per pane (alacritty_terminal), screen
snapshots, snapshot→VT diff encoding, scrollback store. Pure, synchronous,
no tokio. Also usable client-side as a mirror (desktop prediction).

```rust
#[derive(Debug, Clone)]
pub struct TermConfig {
    pub size: TermSize,
    /// Lines kept in the ScrollbackStore. Default 10_000.
    pub scrollback_limit: usize,
}
impl Default for TermConfig { /* 80x24, 10_000 */ }

#[derive(Debug, thiserror::Error)]
pub enum TermError {
    #[error("invalid size {0:?}")]
    InvalidSize(TermSize),
}

/// One pane's terminal. `Send`, not `Sync`: civet-session keeps it in a Mutex.
pub struct PaneTerminal { /* alacritty_terminal::Term<Listener>, vte parser for extra OSCs, store, counters */ }

impl PaneTerminal {
    pub fn new(config: TermConfig) -> Self;

    /// Parse PTY output. Any visible change (cells, cursor, modes, title,
    /// alt-screen, history growth) bumps `state()` by exactly one per call.
    pub fn feed(&mut self, bytes: &[u8]) -> FeedOutcome;

    /// Current state number. Starts at 1 for the initial blank screen.
    pub fn state(&self) -> u64;
    pub fn size(&self) -> TermSize;
    /// Resize the emulator (reflow as alacritty does). Bumps state if changed.
    /// Sizes are clamped to at least 2x1 and at most
    /// `civet_proto::{MAX_TERM_COLS, MAX_TERM_ROWS}` (1000 x 500); Err for 0
    /// cols/rows. `new()`
    /// clamps `TermConfig::size` the same way.
    pub fn resize(&mut self, size: TermSize) -> Result<bool, TermError>;
    pub fn title(&self) -> &str;
    pub fn alt_screen(&self) -> bool;
    /// Total lines ever scrolled off the primary screen (absolute counter).
    pub fn history_lines(&self) -> u64;

    /// Immutable copy of the current screen; cached, so repeated calls at the
    /// same state return the same Arc.
    pub fn snapshot(&mut self) -> std::sync::Arc<Snapshot>;

    /// Diff from `base` to the current state (or Full when `base` is None or
    /// cannot be diffed). Takes a snapshot internally.
    pub fn encode_update(&mut self, base: Option<&Snapshot>) -> EncodedUpdate;
    /// Recent snapshots by state (ring of `DEFAULT_SNAPSHOT_RING` = 2; only
    /// the last fallback of `Subscription::reset_baseline`, a miss is a Full).
    pub fn snapshot_at(&self, state: u64) -> Option<std::sync::Arc<Snapshot>>;
    pub fn set_snapshot_retention(&mut self, n: usize);

    pub fn scrollback(&self) -> &ScrollbackStore;
    /// Serve a ScrollbackQuery (clamped to what is retained).
    pub fn scrollback_page(&self, req: &ScrollbackRequest) -> ScrollbackPage;

    /// Test/debug helper: visible rows as plain text, trailing spaces trimmed.
    pub fn screen_text(&self) -> Vec<String>;
    pub fn cursor(&self) -> CursorState;
}

#[derive(Debug, Default)]
pub struct FeedOutcome {
    /// state() changed during this call.
    pub changed: bool,
    /// Bytes the terminal must answer to the program (DSR/CPR, DA1/DA2,
    /// DECRQM, OSC 10/11 color queries, XTVERSION...): write them to the PTY.
    pub pty_replies: Vec<u8>,
    /// New title if OSC 0/2 changed it.
    pub title: Option<String>,
    /// BEL (0x07 outside of OSC/DCS) seen.
    pub bell: bool,
    /// OSC 9 / OSC 777;notify notifications, in order.
    pub notifications: Vec<OscNotification>,
    /// OSC 133 shell-integration marks, in order.
    pub prompt_marks: Vec<PromptMark>,
    /// OSC 7 working directory (file://host/path → path).
    pub cwd: Option<String>,
    /// alternate screen entered/left during this call (final value).
    pub alt_screen_changed: Option<bool>,
    /// OSC 52 clipboard write (decoded; last one wins), at most
    /// MAX_CLIPBOARD_BYTES. alacritty runs with Osc52::OnlyCopy: queries
    /// (`OSC 52;c;?`) are never answered, so a program cannot read the
    /// client's clipboard.
    pub clipboard: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscNotification {
    /// None for OSC 9; the title field for OSC 777.
    pub title: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    PromptStart,                                 // OSC 133;A
    CommandStart,                                // OSC 133;B
    CommandExecuted,                             // OSC 133;C
    CommandFinished { exit_code: Option<i32> },  // OSC 133;D[;code]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState { pub row: u16, pub col: u16, pub visible: bool, pub shape: CursorShape }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape { #[default] Block, Underline, Bar, BlinkingBlock, BlinkingUnderline, BlinkingBar }

/// Modes that affect how the *client* encodes input or renders; must be
/// mirrored to xterm.js by the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermModes {
    pub app_cursor: bool,        // DECCKM ?1
    pub app_keypad: bool,        // DECKPAM / DECKPNM
    pub bracketed_paste: bool,   // ?2004
    pub mouse: MouseMode,        // ?1000 / ?1002 / ?1003
    pub mouse_sgr: bool,         // ?1006
    pub focus_events: bool,      // ?1004
    pub alt_screen: bool,        // ?1049 / ?1047 / ?47
    pub auto_wrap: bool,         // ?7 (default true in practice)
    pub reverse_video: bool,     // ?5
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode { #[default] Off, Click, Drag, Motion }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color { Default, Indexed(u8), Rgb(u8, u8, u8) }

/// Hand-rolled bit set (no bitflags dependency needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellFlags(pub u16);
impl CellFlags {
    pub const BOLD: Self; pub const DIM: Self; pub const ITALIC: Self;
    pub const UNDERLINE: Self; pub const DOUBLE_UNDERLINE: Self; pub const UNDERCURL: Self;
    pub const BLINK: Self; pub const INVERSE: Self; pub const HIDDEN: Self;
    pub const STRIKETHROUGH: Self;
    /// First half of a wide (2-cell) char.
    pub const WIDE: Self;
    /// Second half of a wide char (renders nothing).
    pub const WIDE_SPACER: Self;
    pub fn contains(self, other: Self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    /// Zero-width / combining characters following `ch`.
    pub zerowidth: Option<Box<str>>,
    pub fg: Color,
    pub bg: Color,
    /// Underline color (SGR 58), None = same as fg.
    pub underline_color: Option<Color>,
    pub flags: CellFlags,
    /// OSC 8 hyperlink URI, if any.
    pub hyperlink: Option<std::sync::Arc<str>>,
}

/// Immutable screen state. Rows are shared (Arc) between snapshots when
/// unchanged, so retaining several snapshots is cheap.
pub struct Snapshot { /* .. */ }

impl Snapshot {
    pub fn state(&self) -> u64;
    pub fn size(&self) -> TermSize;
    pub fn history_lines(&self) -> u64;
    pub fn alt_screen(&self) -> bool;
    pub fn title(&self) -> &str;
    pub fn cursor(&self) -> CursorState;
    pub fn modes(&self) -> TermModes;
    /// 0-based visible row/col; None if out of range.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell>;
    pub fn row_text(&self, row: u16) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedUpdate {
    pub kind: UpdateKind,
    pub state: u64,
    pub base_state: u64,      // 0 for Full
    pub size: TermSize,
    pub history_lines: u64,
    pub history_start: u64,
    pub payload: Vec<u8>,
}

impl EncodedUpdate {
    pub fn into_screen_update(self, pane: PaneId, acked_input_seq: u64) -> ScreenUpdate;
}

/// Free functions (PaneTerminal::encode_update is a convenience wrapper):
pub fn encode_full(to: &Snapshot, store: &ScrollbackStore) -> EncodedUpdate;
pub fn encode_diff(from: &Snapshot, to: &Snapshot, store: &ScrollbackStore) -> EncodedUpdate;
/// Repaint cells `cols` of visible `row` exactly as in `snap` (attributes
/// included), preserving the cursor position (DECSC … DECRC) and leaving SGR
/// reset. Used by client-side prediction to undo predicted cells.
pub fn encode_cells(snap: &Snapshot, row: u16, cols: std::ops::Range<u16>) -> Vec<u8>;

/// Absolute-numbered history lines, VT-encoded (compact: SGR changes only
/// where attributes change). Evicts oldest beyond `limit`.
pub struct ScrollbackStore { /* VecDeque<ScrollbackLine>, first: u64 */ }

impl ScrollbackStore {
    pub fn new(limit: usize) -> Self;
    pub fn push(&mut self, line: ScrollbackLine);
    /// Oldest retained absolute line number.
    pub fn oldest(&self) -> u64;
    /// One past the newest (== history_lines).
    pub fn end(&self) -> u64;
    pub fn get(&self, line: u64) -> Option<&ScrollbackLine>;
    pub fn range(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &ScrollbackLine)>;
    pub fn page(&self, req: &ScrollbackRequest) -> ScrollbackPage;
}
```

Payload contract (what `encode_full` / `encode_diff` must produce; see also
`ScreenUpdate` rustdoc):

- **Full**: `ESC c` (RIS: clears screen *and* xterm.js scrollback, resets
  modes), then replay history lines
  `max(store.oldest(), history_lines - FULL_REPLAY_LINES)..history_lines`
  into the scrollback, then set modes, paint every visible cell, set title
  (OSC 2), cursor shape (DECSCUSR), position and visibility; SGR reset at the
  end. `history_start` = first replayed line.
- **Byte budget** (both kinds): the replay is also capped at
  `civet_term::MAX_REPLAY_BYTES` (8 MiB, VT bytes); the *oldest* lines are
  dropped first and `history_start` advances, so the client sees a gap it
  fills with a scrollback fetch. Every payload is at most
  `civet_term::MAX_UPDATE_BYTES` (12 MiB, below `MAX_FRAME_LEN`): a Diff over
  it is replaced by a Full; a Full over it drops the replay, then as a last
  resort repaints text only (no SGR/hyperlinks/combining marks, title cut to
  1 KiB). Callers can rely on `encode_*` output always fitting in a frame.
- **Diff** (`from` → `to`): applied to an xterm.js whose *visible buffer*
  equals `from`, results in `to`, and appends to scrollback exactly lines
  `history_start..to.history_lines`, where `history_start =
  max(from.history_lines, to.history_lines - MAX_REPLAY_LINES, store.oldest())`.
  - If no history growth and same size/screen buffer: rewrite only changed
    cells/rows (cursor moves + minimal SGR + text; use `EL` for blank tails).
  - If history grew (output scrolled): suggested encoding is clear the visible
    screen without pushing it to scrollback (`CSI H`, `CSI 2 J`; xterm.js's
    `scrollOnEraseInDisplay` is false by default), print the replayed lines
    top-down each followed by CRLF (soft-wrapped lines: print full width, no
    CRLF, let autowrap continue), then `rows - 1` more CRLF at the bottom row to
    push the remaining replayed lines out, then repaint `to` fully. Any
    encoding is fine if it meets the contract.
  - Size change: payload assumes the client already resized to `to.size`
    (client resizes before writing); repaint fully, still diff-replay
    history.
  - Alt-screen switch: emit `CSI ?1049h`/`l` (cursor save/restore semantics as
    xterm) then repaint the now-visible buffer fully.
  - Modes: emit set/reset only for modes that differ.
  - Wide chars occupy two cells; never split them. Combining chars follow
    their base char.
- Payloads are UTF-8 except for intentional C0/C1 controls; never emit
  8-bit C1 bytes (use 7-bit ESC forms).
- `ScrollbackLine.vt`: the row's text with inline SGR, ending in `CSI 0 m`, no
  newline; trailing blank cells trimmed; `wrapped = true` if the row
  soft-wraps into the next.

State numbering: `state()` increments by one per `feed()`/`resize()` call that
changed anything (not per byte). It is never reused; snapshots compare by it.

**Scrollback capture with alacritty_terminal** (recommended, exact): set
alacritty's `scrolling_history` to a fixed H (e.g. 1000). Feed input in chunks
that cannot scroll more than H-1 lines (count LF/VT/FF/IND/NEL, CSI S
parameters, plus `len / cols` for autowrap; split when the bound reaches H/2).
After each chunk, copy `grid.history_size()` new lines (oldest first) into the
`ScrollbackStore` and call `grid.clear_history()`. The alt screen has no
history. On resize, alacritty can pull history back into view on grow; with
cleared history it pulls nothing, which is acceptable.

Tests to include (property tests strongly recommended): for random VT input
split at random points, `encode_full(to)` fed into a fresh `PaneTerminal`
reproduces `to` (`screen_text`, cursor, modes); `encode_full(from)` +
`encode_diff(from, to)` fed into a fresh `PaneTerminal` reproduces `to`;
`seq 1 100000` produces contiguous history lines `"1".."N"` with no gaps or
duplicates; DSR `CSI 6 n` yields a CPR in `pty_replies`; OSC 9 / 777 / 133 / 7
are reported; wide chars and emoji survive diffs.

Performance budget: `feed` ≥ 50 MB/s of typical output; `encode_diff` for a
200×60 screen with a few changed cells < 100 µs; full repaint < 1 ms.

---

## 4. civet-session

Owns repos/worktrees (git CLI), PTYs (portable-pty) feeding civet-term,
input seq tracking, per-client subscriptions with mosh-style flow control,
and agent status detection. No networking.

```rust
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// `~/.civet` (or $CIVET_STATE_DIR). Holds repos.json and status/.
    pub state_dir: std::path::PathBuf,
    /// Shell for panes without a command and for `-lc` wrapping.
    /// Default: $SHELL, else /bin/bash, else /bin/sh.
    pub shell: Option<String>,
    /// Per-pane scrollback store limit. Default 10_000.
    pub scrollback_limit: usize,
    /// Quiet period before an agent pane goes Running -> Idle. Default 8 s.
    pub idle_after: std::time::Duration,
    /// Path of the git binary. Default "git".
    pub git: std::path::PathBuf,
    /// Target of the `hooks/bin/civet` pane shim (the daemon's binary).
    /// None = no shim. Default None.
    pub cli_program: Option<std::path::PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("git: {0}")]
    Git(String),        // include git's stderr (trimmed)
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Backpressure from `write_input`: the pane's PTY queue is full. The
    /// input was not applied or recorded; await `wait_input_capacity` and
    /// retry the same message.
    #[error("busy: {0}")]
    Busy(String),
}

impl From<&SessionError> for RpcError { /* NotFound->NotFound, ..., Io->Io, Busy->Internal */ }
impl From<SessionError> for RpcError { /* same */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome { Applied, Duplicate }

/// Clone = same manager (Arc inside). Send + Sync.
#[derive(Clone)]
pub struct SessionManager { /* .. */ }

impl SessionManager {
    /// Loads `<state_dir>/repos.json` (missing = empty), creates
    /// `<state_dir>/status/`. Must be called inside a tokio runtime.
    pub fn new(config: SessionConfig) -> Result<Self, SessionError>;

    /// All daemon events (pane lifecycle, titles, status, bells, repo
    /// changes). Lagging receivers lose events (broadcast); capacity 1024.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<Event>;

    // ----- repos & worktrees (git CLI, run via tokio::process) -----
    /// Git repositories only.
    pub async fn list_repos(&self) -> Result<Vec<RepoInfo>, SessionError>;
    /// Repositories and folder projects.
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>, SessionError>;
    /// `git -C path rev-parse --path-format=absolute --git-common-dir
    /// --show-toplevel` to find the repo; RepoId = hex of the first 8 bytes
    /// of SHA-256(canonical common git dir). Persists repos.json. Emits
    /// ReposChanged. AlreadyExists if registered.
    pub async fn add_repo(&self, path: &str) -> Result<RepoInfo, SessionError>;
    /// add_repo, or a directory in no git repository as a folder project.
    pub async fn add_project(&self, path: &str) -> Result<ProjectInfo, SessionError>;
    pub async fn remove_repo(&self, repo: &RepoId) -> Result<(), SessionError>;
    /// `git worktree list --porcelain`, minus bare and prunable (directory
    /// deleted) entries.
    pub async fn list_worktrees(&self, repo: &RepoId) -> Result<Vec<WorktreeInfo>, SessionError>;
    /// `git worktree add [-b branch] <path> [base]`. Default path:
    /// `<parent of main worktree>/<repo name>-worktrees/<branch, '/' -> '-'>`.
    /// If `base` is None and `branch` exists: check it out; if it does not
    /// exist: create it from HEAD. With `base`, an existing `branch` is
    /// checked out only if it points at `base` and no worktree has it, else
    /// AlreadyExists. Prunes stale registrations first. Emits WorktreesChanged.
    pub async fn create_worktree(
        &self, repo: &RepoId, branch: &str, base: Option<&str>, path: Option<&str>,
    ) -> Result<WorktreeInfo, SessionError>;
    /// `git worktree remove [--force]` (`-f -f` for a locked worktree when
    /// forced). Refuses the main worktree. A worktree whose directory is
    /// already gone is pruned. Emits WorktreesChanged.
    pub async fn remove_worktree(&self, worktree: &WorktreeId, force: bool) -> Result<(), SessionError>;

    // ----- files (paths relative to the worktree, never outside it) -----
    /// One directory level; gitignored entries (one `git check-ignore
    /// --stdin` call) left out unless `include_ignored`.
    pub async fn list_files(&self, worktree: &WorktreeId, dir: &str, include_ignored: bool)
        -> Result<DirListing, SessionError>;
    /// The first `max_bytes` (capped at 1 MiB) of a regular file.
    pub async fn read_file(&self, worktree: &WorktreeId, path: &str, max_bytes: u64)
        -> Result<FilePreview, SessionError>;
    /// `git ls-files -z -co --exclude-standard`, sorted, capped.
    pub async fn list_all_files(&self, worktree: &WorktreeId) -> Result<FileList, SessionError>;
    /// Up to `len` bytes (capped at 8 MiB) of a regular file from `offset`.
    pub async fn read_file_chunk(&self, worktree: &WorktreeId, path: &str, offset: u64, len: u64)
        -> Result<FileChunk, SessionError>;
    /// Create / move / copy / write / trash / restore (see the "File ops" protocol row);
    /// trash lives in `<state_dir>/trash`.
    pub async fn file_op(&self, worktree: &WorktreeId, op: FileOp)
        -> Result<FileOpResult, SessionError>;
    /// Subdirectories of any host directory, git repositories flagged
    /// (see the "Folder completion" protocol row).
    pub async fn list_dirs(&self, path: &str) -> Result<HostDirListing, SessionError>;

    // ----- panes -----
    /// Spawns the PTY (see "Process launch" below), starts the reader thread
    /// and status detection. Emits PaneCreated.
    pub async fn spawn_pane(&self, req: SpawnPaneRequest) -> Result<PaneInfo, SessionError>;
    /// create_worktree(repo, branch, base or default branch, worktree_path)
    /// with a new branch (an existing one only as create_worktree allows), then spawn_pane in it
    /// with `command`, `title.unwrap_or(branch)`, `agent`.
    pub async fn launch_agent(&self, req: LaunchAgentRequest)
        -> Result<(WorktreeInfo, PaneInfo), SessionError>;
    pub fn list_panes(&self) -> Vec<PaneInfo>;
    pub fn pane_info(&self, pane: PaneId) -> Result<PaneInfo, SessionError>;
    /// PTY resize (TIOCSWINSZ via portable-pty) + terminal resize. Emits
    /// PaneResized. Last caller wins.
    pub fn resize_pane(&self, pane: PaneId, size: TermSize) -> Result<(), SessionError>;
    /// Signal the process group, close the PTY, drop the pane. Emits
    /// PaneClosed. Subscriptions for it end (`next()` returns None).
    pub fn kill_pane(&self, pane: PaneId, signal: KillSignal) -> Result<(), SessionError>;
    pub fn set_agent_status(&self, pane: PaneId, status: AgentStatus) -> Result<(), SessionError>;
    pub fn fetch_scrollback(&self, req: &ScrollbackRequest) -> Result<ScrollbackPage, SessionError>;

    // ----- input -----
    /// Apply `msg` iff `msg.seq > last_input_seq(client, pane)`, then record
    /// seq and the time written (for echo-ack). Gaps are allowed (logged).
    /// Writes go through a per-pane writer thread/channel (never block the
    /// caller on a full PTY). Error if the pane exited.
    /// Byte budget: while more than 1 MiB is queued for the PTY (the process
    /// is not reading stdin), returns `Err(Busy)` without recording the seq;
    /// after a Busy, later seqs from that client are also Busy until the
    /// refused seq (`last_input_seq + 1`) is retried, so input is never
    /// reordered or lost. A single message larger than the budget is
    /// accepted when the queue is empty.
    pub fn write_input(&self, client: ClientId, msg: &InputMessage) -> Result<InputOutcome, SessionError>;
    /// Resolves once the pane's PTY queue is at or below 512 KiB, or the
    /// pane's writer is gone (exit/kill/unknown pane). The daemon awaits this
    /// in the connection's input path and then retries: that stalls reading
    /// the client's control stream, so flow control pushes back to the client.
    pub async fn wait_input_capacity(&self, pane: PaneId);
    pub fn last_input_seq(&self, client: ClientId, pane: PaneId) -> u64;
    /// Forget all per-client input seqs (fresh, non-resumed session).
    pub fn reset_client(&self, client: ClientId);

    // ----- screen subscriptions -----
    /// Per-(client, pane) subscription. The daemon keeps it across
    /// reconnects of a resumed session.
    pub fn subscribe(&self, client: ClientId, pane: PaneId) -> Result<Subscription, SessionError>;

    /// Kill all panes (SIGHUP), stop threads.
    pub async fn shutdown(&self);
}

/// Handle shared between the daemon's per-pane send task and its control
/// message handler. Clone = same subscription. All methods take &self.
#[derive(Clone)]
pub struct Subscription { /* Arc<Mutex<SubState>> + Notify + watch of pane state */ }

pub enum PaneOutput {
    Screen(ScreenUpdate),
    /// Input applied >= ECHO_ACK_DELAY_MS ago without a screen update that
    /// carried its ack.
    InputAck { pane: PaneId, seq: u64 },
}

impl Subscription {
    pub fn pane(&self) -> PaneId;
    pub fn client(&self) -> ClientId;
    /// Current SubscribeInfo (state, size, history_lines, last_input_seq).
    pub fn info(&self) -> SubscribeInfo;
    /// Set the diff baseline: the snapshot for `known_state` if this
    /// subscription still holds it (last acked, or in flight), else none
    /// (next update is Full). Clears the in-flight queue. Called on every
    /// SubscribePane and when a new connection takes over the subscription.
    pub fn reset_baseline(&self, known_state: Option<u64>);
    /// Wait for the next thing to send. Returns None when the pane is closed.
    /// Waits while `inflight.len() >= MAX_INFLIGHT_UPDATES`. Coalesces:
    /// after a change wakes it, waits up to 2 ms for more output when the
    /// pane was idle, and sends at most one update per 8 ms per pane under
    /// continuous output; always encodes from baseline to the *current* state
    /// (newest-wins). Cancel-safe.
    pub async fn next(&self) -> Option<PaneOutput>;
    /// Client applied `state`: release in-flight snapshots <= state; the
    /// acked snapshot becomes the resume baseline.
    pub fn ack(&self, state: u64);
}
```

Mosh-style details inside `Subscription`:

- `baseline`: snapshot the next diff is relative to = the last snapshot
  **sent** on the current connection (streams are reliable), or after
  `reset_baseline`, the client's `known_state` snapshot if held.
- `inflight: VecDeque<Arc<Snapshot>>` sent but unacked; `acked:
  Option<Arc<Snapshot>>`. `reset_baseline(Some(s))` searches `acked` and
  `inflight` for `s`.
- `acked_input_seq` in an update = highest seq from this client for this
  pane written at least `ECHO_ACK_DELAY_MS` before the snapshot was taken.

Process launch (`spawn_pane`):

- cwd: `req.cwd`, else the worktree path, else `$HOME`. Must exist
  (InvalidArgument otherwise).
- argv: no command → `[$SHELL, "-l"]`. With command →
  `[$SHELL, "-lc", "exec \"$0\" \"$@\"", program, args...]` so login PATH
  applies and the agent replaces the shell (its exit = pane exit).
- env: daemon env plus `TERM=xterm-256color`, `COLORTERM=truecolor`,
  `CIVET_PANE_ID=<id>`, `CIVET_STATUS_FILE=<state_dir>/status/<id>.status`
  (file created empty), `CIVET_WORKTREE=<path>` when a worktree is set, then
  `req.env` (overrides).
- The child gets its own session/process group (portable-pty does setsid);
  `KillPane` signals the group (`libc::killpg`).
- One reader thread per pane: blocking `read()`, then keep reading while a
  zero-timeout `poll()` says more is ready (no timers, fd stays blocking),
  up to 64 KiB per batch → lock terminal → `feed()` → write `pty_replies`
  back (dropped while more than 64 KiB is queued for the PTY, so a program
  flooding DA/DSR queries without reading stdin cannot grow the daemon) →
  run status detection → handle title/bell/OSC → publish new state on a
  `watch::Sender<u64>`. A waiter thread
  `wait()`s for exit → `PaneExited`, status Done.
- Exited panes stay listed (screen and scrollback readable) until
  `kill_pane`.

Agent status detection (per pane; emits `Event::AgentStatus` only on change,
updates `PaneInfo.status`):

| Signal | Applies to | Result | Source |
|---|---|---|---|
| Process exit | all panes | `Done` (message `"exit <code>"`) | ProcessExit |
| Hook file `$CIVET_STATUS_FILE` changed (poll inode+mtime+len every 250 ms, read at most `MAX_STATUS_FILE_BYTES` = 4 KiB, parse with `civet_proto::parse_status_file`: message control chars stripped, truncated to `MAX_STATUS_MESSAGE_CHARS` = 500) | all panes (makes the pane an agent pane) | parsed status + message | Hook |
| OSC 9 / OSC 777 notification | panes without hook reports | `NeedsInput` if body matches `/permission\|approve\|allow\|waiting\|input\|confirm\|\?$/i`, else `Done`; message = body | EscapeCode |
| OSC 133;C | all panes | `Running` | EscapeCode |
| OSC 133;D / 133;A | non-agent panes | `Idle` | EscapeCode |
| BEL, then no output for 1 s | agent panes, status Running | `NeedsInput` | Heuristic |
| Output not attributable to echo (more than 64 bytes within 1 s, ignoring output within 150 ms after input) | agent panes | `Running` | Heuristic |
| No output for `idle_after` | agent panes without hooks, status Running | `Done` if the run lasted ≥ 10 s and the pane never had a hook report, else `Idle` | Heuristic |
| No output for 30 s | panes with a hook `running`/`needs_input`, status Running | `Idle` (interrupted turn) | Heuristic |
| Input, then > 64 bytes of output within 2 s | agent panes, status NeedsInput | `Running` (prompt answered) | Heuristic |
| `SetAgentStatus` | all | given | Client |

Precedence: once a pane has received a Hook `running` or `needs_input`, heuristics stop
applying to it except the two fallbacks above (EscapeCode marks,
ProcessExit and Client still apply). A pane with only `done` hook reports
(Codex `notify`) keeps the heuristics. "Agent
pane" = `agent.is_some()` or the program basename is one of `claude`,
`codex`, `gemini`, `aider`, `opencode`, `amp`, `cursor-agent`, `goose`.

Agent hook installation: the session writes `<state_dir>/hooks/`
(`civet-status-hook.sh`, `claude-settings.json`, PATH shims `bin/claude` and
`bin/codex`, `bin/civet` when `SessionConfig::cli_program` is set (the
daemon sets it to its own binary: `CIVET_STATE_DIR=<state> exec <civetd>
"$@"`), zsh `ZDOTDIR` shim `zsh/.zshenv`). Every pane gets
`CIVET_SHIM_DIR=<state_dir>/hooks/bin` first on `PATH`, `TERM_PROGRAM=civet`,
and shell integration that keeps the shim dir first after the user's rc
files (zsh `ZDOTDIR` + `precmd`, bash `PROMPT_COMMAND`, fish `-C`; `-lc`
commands are prefixed with `PATH="$CIVET_SHIM_DIR:$PATH"; `). The `claude`
shim execs the real `claude --settings <claude-settings.json>` unless
`--settings` is given; the `codex` shim adds `-c notify=[<hook>,"done",
"--codex-notify"]` unless the user configured `notify`. Neither touches
`~/.claude` / `~/.codex`. Claude hooks: `UserPromptSubmit`/`PostToolUse` →
`running`, `PreToolUse` → `running` "soft" (never replaces a pending
`needs_input`), `Notification` → `needs_input` + message (`idle_prompt` /
`auth_success` ignored), `Stop` → `done`. Argv launches of an explicit-path
`claude` (or the `claude` agent hint on another launcher) get `--settings`
appended to argv. Details: `crates/civet-session/HOOKS.md`.

Worktrees: `create_worktree` validates `base` with `git rev-parse --verify
--quiet --end-of-options <base>^{commit}` and passes `--` before the path; a
newly created branch gets only `branch.<b>.remote=origin` /
`branch.<b>.merge=refs/heads/<b>` (when `origin` exists), never repo-wide
`push.*` config.

Tests to include: spawn `sh -c 'printf hello'` and see "hello" in a
subscription's Full update; input dedup (same seq twice → Duplicate);
coalescing under `yes | head -c 10M`; flow control never exceeds
`MAX_INFLIGHT_UPDATES`; `reset_baseline` with an in-flight state yields a
`Diff`; worktree create/list/remove against a temp repo (`git init` in
`tempfile`); hook file → AgentStatus event; process exit → Done.

---

## 5. civetd

Crate `civetd`: library (`src/lib.rs`) + binaries `civetd` (needs feature
`server`, default on) and `civet-cli`. `civet-desktop` depends on it with
`default-features = false` for the client `bootstrap` module only.

```
src/lib.rs           pub mod bootstrap; pub mod paths; #[cfg(feature="server")] pub mod daemon;
src/paths.rs         state-dir helpers (shared)
src/bootstrap.rs     client side: ssh, install/upgrade, parse JSON line, stdio tunnel
src/daemon/mod.rs    serve / bootstrap (server side) / stdio-tunnel
src/main.rs          clap CLI for civetd
src/bin/civet-cli.rs test client
```

### 5.1 `civetd` command line

```
civetd serve        [--state-dir DIR] [--bind IP] [--port-range 60001-60999] [--detach]
civetd bootstrap    [--state-dir DIR] [--replace] [--bind IP]
civetd stdio-tunnel [--state-dir DIR]
civetd version      [--json]
civetd handoff      (--prompt TEXT | --prompt-file FILE|-) [--agent claude|codex] [--title T]
                    [--branch B] [--base REV] [--from DIR] [--carry-changes]
                    [--outside-pane] [--state-dir DIR] [-- CMD ARGS…]
civetd install-skills [--claude] [--codex]
```

- **serve**: take an exclusive `flock` on `<state>/civetd.lock` (exit 3 if
  held); generate a new `SessionKey`; `ServerIdentity::load_or_generate(<state>)`;
  `QuicServer::bind_port_range` (default `0.0.0.0`, `60001-60999`; env
  `CIVET_PORT_RANGE` overrides); listen on unix socket `<state>/civetd.sock`
  (mode 0600; remove a stale one first); write `<state>/daemon.json` =
  `BootstrapInfo` JSON (mode 0600, temp + rename) **last**, so its existence
  means "ready". `--detach`: `setsid`, stdio → `/dev/null`, logs to
  `<state>/logs/civetd.log` (tracing-subscriber, `CIVET_LOG` env filter,
  default `info`). On SIGTERM/SIGINT: send `ServerMessage::Goodbye` to all
  clients, `SessionManager::shutdown`, remove `daemon.json` and the socket.
- **bootstrap** (run over SSH by clients): if `daemon.json` exists, its pid is
  alive and the unix socket answers a hello+Ping+ListPanes within 2 s →
  print its line with `reattached: true` and exit 0 (the flock is only
  probed when no healthy daemon is advertised, and never while waiting for
  a just-started `serve`, which could otherwise lose the lock race). The
  printed line also carries `available_version` (this binary), `live_panes`
  and `upgrade_available` (an older compatible daemon was kept because it
  has panes; `--replace` restarts it). `--bind IP` is passed to a `serve`
  it starts (local clients use 127.0.0.1). If that daemon's
  `proto_version` is incompatible (or `--replace`): if it has no panes
  (checked via `ListPanes`), SIGTERM it and wait up to 5 s; if it has panes
  and no `--replace`, exit 4 with a message on stderr. Otherwise spawn
  `current_exe() serve --detach` with the same `--state-dir`, wait up to
  5 s for `daemon.json`, print it as one JSON line on stdout
  (`BootstrapInfo::to_json_line`), exit 0. Nothing else goes to stdout;
  diagnostics go to stderr.
- **stdio-tunnel**: connect to `<state>/civetd.sock` and copy bytes both ways
  between it and stdin/stdout (`tokio::io::copy_bidirectional` on a joined
  stdin/stdout); exit when either side closes. Exit 2 if no daemon is running.
- **handoff** (client; panes run it as `civet handoff` through the
  `hooks/bin/civet` shim): requires `CIVET_PANE_ID` unless `--outside-pane`;
  reads `<state>/daemon.json` and connects to `<state>/civetd.sock` (never
  starts a daemon); finds the git checkout of `--from` (default cwd), `ListRepos`
  / `AddRepo` its main worktree, `CreateWorktree` from `--base` (default
  its `HEAD`) on `--branch` or a slug of the title (suffix `-2`… on
  `AlreadyExists`), optionally applies the uncommitted changes
  (`--carry-changes`; `RemoveWorktree` if that fails), then `SpawnPane` in
  the worktree running `<agent> <prompt>` (or `CMD ARGS… <prompt>`), titled
  `--title` or the prompt's first line. Prints one JSON object
  `{repo, worktree, branch, base, pane, title, agent, carriedChanges}`.
  See `docs/HANDOFF.md`.
- **install-skills**: write the embedded `civet-handoff` `SKILL.md` to
  `~/.claude/skills/civet-handoff/` and/or
  `${CODEX_HOME:-~/.codex}/skills/civet-handoff/`; prints the paths.
- **version**: `civetd 0.1.0 (proto 1, x86_64-unknown-linux-musl)`; `--json`
  prints `{"version":"0.1.0","proto_version":1,"target":"..."}`. The target
  triple comes from a `build.rs` exporting `TARGET` (add one).

Exit codes: 0 ok, 1 generic error, 2 no daemon, 3 already running,
4 incompatible daemon has live panes.

### 5.2 Daemon wiring (`daemon` module, feature `server`)

```rust
pub struct DaemonOptions {
    pub state_dir: std::path::PathBuf,
    pub bind_ip: std::net::IpAddr,
    pub port_range: std::ops::RangeInclusive<u16>,
}
pub async fn serve(opts: DaemonOptions) -> anyhow::Result<()>;
pub async fn bootstrap(state_dir: &std::path::Path, replace: bool, bind: Option<std::net::IpAddr>) -> anyhow::Result<civet_proto::BootstrapInfo>;
pub async fn stdio_tunnel(state_dir: &std::path::Path) -> anyhow::Result<()>;
```

Per accepted connection (QUIC or unix socket, same code):

1. `handshake()` → `ServerConnection` with `hello`.
2. Look up `ClientState` by `hello.client_id` in a daemon-wide map. Resume iff
   `hello.resume` matches its current or previous token (`ct_eq`). If
   resumed: close the client's old connection (code 3), keep subscriptions
   (paused; their send tasks were stopped), reply `resumed: true` with
   `resumed_subscriptions = subs.map(Subscription::info)`. If not resumed:
   drop old subscriptions, `SessionManager::reset_client`, `resumed: false`.
   Rotate the token (keep the previous as "previous"). `instance_id` = random
   u64 chosen at daemon start.
3. Forward `SessionManager::subscribe_events()` to the client as
   `ServerMessage::Event`.
4. For each `ServerIncoming`:
   - `Request` → dispatch to `SessionManager`, reply `ControlResponse`. Run
     slow ones (git, spawn) in spawned tasks so input is never stuck behind
     them.
   - `SubscribePane { pane, known_state }` → get or create the
     `Subscription`, `reset_baseline(known_state)`, (re)start its send task
     (`open_screen_stream(pane)` then loop `next()` → `ScreenStream::send` or
     `ServerSender::send(InputAck)`), reply `Subscribed(info)`.
   - `Input` → `write_input(client_id, &msg)`. `SessionError::Busy` (PTY
     write backlog full) is neither applied nor acked; the client
     retransmits it.
   - `Ack` → `subscription.ack(state)`.
   - `Scrollback` → `responder.respond(sessions.fetch_scrollback(..))`.
   - `Goodbye` → drop the `ClientState` immediately.
5. On disconnect: stop send tasks, keep `ClientState` for 24 h (resume TTL).
   Expiry checks `conn.is_none()` and the TTL and removes the state under
   one lock.

Hardening:

- At most `MAX_PENDING_HANDSHAKES` (32) connections per listener may be in
  their unauthenticated handshake + hello; more are refused.
- `ResizePane` / `SpawnPane` / `LaunchAgent` sizes are capped at
  `MAX_TERM_COLS` x `MAX_TERM_ROWS` before they reach the session.
- Event forwarding: an event that cannot be encoded is logged and skipped
  (only a closed connection ends the task). If the connection lags behind
  the daemon's event broadcast, the missed events are replaced by a resync:
  `PaneClosed` for every pane the client was told about that is gone,
  `PaneCreated(info)` for every live pane (an upsert carrying title, size,
  status, exit), then `ReposChanged`.
- Screen send task: an update that cannot be framed (too large) is not
  fatal: `reset_baseline(None)` and send a Full instead (backing off if it
  keeps failing).

### 5.3 Client bootstrap module (`civetd::bootstrap`, no `server` feature)

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshTarget {
    /// `host`, `user@host` or an ssh_config alias.
    pub destination: String,
    pub port: Option<u16>,
    /// Extra ssh args, e.g. ["-i", "~/.ssh/id_ed25519", "-J", "bastion"].
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum BinarySource {
    /// Directory with `civetd-<target-triple>` files (desktop app resources,
    /// or `CIVET_DIST_DIR`).
    Dir(std::path::PathBuf),
    /// Exactly this binary (must match the remote's triple).
    File(std::path::PathBuf),
    /// `civetd` next to the current executable (dev: same-OS remotes only).
    CurrentExeSibling,
    /// Try in order; first file with the right embedded version wins.
    Search(Vec<BinarySource>),
}
// BinarySource::from_env() = Search[bundled `civet-dist` resource dir (if
// any), $CIVET_DIST_DIR, ~/.civet/dist, CurrentExeSibling];
// BinarySource::with_bundled(Some(dir)) puts an explicit resource dir first.
// The chosen file is logged (tracing info). Every civetd binary embeds
// `\0CIVETD_VERSION=<version>\0` (bootstrap::VERSION_MARKER,
// bootstrap::binary_version, bootstrap::check_binary).

#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    pub target: SshTarget,
    pub ssh_program: std::path::PathBuf,   // "ssh"
    pub binary: BinarySource,
    /// Remote state dir; None = remote default (~/.civet).
    pub remote_state_dir: Option<String>,
    pub replace: bool,
    /// Per ssh invocation. Default 30 s.
    pub timeout: std::time::Duration,
}

#[derive(Debug, Clone)]
pub struct Bootstrapped {
    pub info: civet_proto::BootstrapInfo,
    /// IP to dial for QUIC: `hostname` from `ssh -G`, resolved.
    pub dial_addr: std::net::SocketAddr,
    /// Remote path of the civetd binary that is running (for stdio-tunnel).
    pub remote_binary: String,
    /// True if this call uploaded a binary.
    pub installed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("ssh failed (exit {code:?}): {stderr}")]
    Ssh { code: Option<i32>, stderr: String },
    #[error("no civetd binary for {triple} (looked in {searched})")]
    NoBinary { triple: String, searched: String },
    #[error("unsupported remote platform: {0}")]
    UnsupportedPlatform(String),
    #[error("remote daemon is incompatible and has live panes; retry with replace")]
    IncompatibleDaemonBusy,
    #[error(transparent)]
    Proto(#[from] civet_proto::ProtoError),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    /// Local dist file has the wrong version / platform (tells the user to
    /// run scripts/build-remote.sh --install). Nothing uploaded.
    WrongBinary { path: String, expected: String, got: String },
    /// Uploaded file failed `civetd version` on the remote (removed again).
    UploadRejected { expected: String, got: String },
    /// Remote state dir or bin/ is a symlink / not owned by the login user.
    UnsafeRemoteDir(String),
    /// Remote path contains ! \ ' " ` or control characters.
    InvalidRemotePath(String),
}

/// Progress callback steps (desktop shows them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStep { Probe, Upload, Start, Resolve }

pub async fn bootstrap_ssh(
    opts: &BootstrapOptions,
    progress: &(dyn Fn(BootstrapStep) + Send + Sync),
) -> Result<Bootstrapped, BootstrapError>;

/// Run `<civetd> bootstrap [--state-dir]` as a local child (no ssh); dial
/// address 127.0.0.1. Used by `civet-cli --local` and desktop "Local".
/// Runs `bootstrap --bind 127.0.0.1` (a started daemon is loopback-only).
pub async fn bootstrap_local(
    civetd: &std::path::Path,
    state_dir: Option<&std::path::Path>,
) -> Result<Bootstrapped, BootstrapError>;
/// Same with `--replace` (restart after `upgrade_available`). Over SSH set
/// `BootstrapOptions::replace`.
pub async fn bootstrap_local_with(
    civetd: &std::path::Path,
    state_dir: Option<&std::path::Path>,
    replace: bool,
) -> Result<Bootstrapped, BootstrapError>;

/// Spawn `ssh <target> <remote_binary> stdio-tunnel` and return its
/// stdin/stdout joined as one AsyncRead + AsyncWrite (kill on drop).
pub async fn open_ssh_tunnel(
    opts: &BootstrapOptions,
    remote_binary: &str,
) -> Result<SshTunnel, BootstrapError>;

pub struct SshTunnel { /* Child + ChildStdout + ChildStdin */ }
impl tokio::io::AsyncRead for SshTunnel { /* .. */ }
impl tokio::io::AsyncWrite for SshTunnel { /* .. */ }

/// Map `uname -s` + `uname -m` to a target triple:
/// Linux x86_64 -> x86_64-unknown-linux-musl, Linux aarch64 ->
/// aarch64-unknown-linux-musl, Darwin arm64 -> aarch64-apple-darwin,
/// Darwin x86_64 -> x86_64-apple-darwin.
pub fn target_triple(uname_s: &str, uname_m: &str) -> Result<String, BootstrapError>;
```

Bootstrap procedure:

1. Every ssh call uses `-o BatchMode=yes -o ControlMaster=auto
   -o 'ControlPath="~/.civet/ssh/%C"' -o ControlPersist=60` (create
   `~/.civet/ssh` mode 0700 locally; the path is double-quoted for ssh with
   `%` doubled; if the socket path would exceed the unix socket limit, 104
   bytes on macOS, sharing is turned off with a warning:
   `ControlMaster=no`, `ControlPath=none`) so the 2–3 calls and a later stdio
   tunnel share one SSH login. Desktop never prompts for passwords; key/agent
   auth is required in the MVP.
2. **Probe** (one ssh call; POSIX sh script passed as the remote command):
   print `uname -s`, `uname -m`, then whether
   `~/.civet/bin/civetd-<our version>` exists, is executable and owned by the
   user in a non-symlink owned state dir, and its `version` output starts
   with `civetd <our version> `. If so, the same script runs it with
   `bootstrap` and its stdout is parsed with `BootstrapInfo::find_in_output`
   (done in one round trip); otherwise (stale, truncated, wrong arch) it is
   reinstalled. Remote paths with `! \ ' " \``, newlines or control
   characters are refused (csh/fish login shells).
3. **Upload** if missing: pick the binary for the triple and check its
   embedded version marker and executable format/arch locally
   (`WrongBinary`), then
   `ssh … 'mkdir -p ~/.civet/bin && cat > ~/.civet/bin/.civetd-<ver>.tmp &&
   chmod 755 … && mv … ~/.civet/bin/civetd-<ver> && ln -sf civetd-<ver>
   ~/.civet/bin/civetd'` with the binary on stdin; the script refuses a
   symlinked / foreign-owned state dir or `bin/` (exit 87,
   `UnsafeRemoteDir`) and runs the temp file's `version` before the `mv`
   (mismatch: remove it, exit 86, `UploadRejected`). Binaries are
   version-suffixed so a running daemon's file is never overwritten.
4. **Start**: `ssh … '~/.civet/bin/civetd-<ver> bootstrap'` (with
   `--replace` / `--state-dir` when requested) and parse the line.
   A reattached older-but-compatible daemon is fine (`info.daemon_version`
   tells the UI).
5. **Resolve**: `ssh -G <destination>` → `hostname` (and `port` is the SSH
   port, irrelevant for QUIC) → `tokio::net::lookup_host((hostname,
   udp_port))`, prefer IPv4. If `-J`/ProxyJump is configured, QUIC will
   likely fail: the caller falls back to the tunnel.

### 5.4 `civet-cli`

```
civet-cli [CONNECT] [--client-id HEX] [--rebind-every SECS] [-v] <COMMAND>

CONNECT (exactly one; default --local):
  --ssh DEST [--ssh-port N] [--ssh-arg ARG]... [--dist DIR] [--replace]
  --local [--civetd PATH] [--state-dir DIR] [--replace]
  --bootstrap-line JSON --host IP       dial a printed bootstrap line directly
  --stdio                               (with --ssh/--local) skip QUIC; use the
                                        stdio tunnel (local: `civetd stdio-tunnel`)

COMMANDS
  ping [--count N] [--interval-ms MS]   RTT per ping, then min/avg/max
  info                                  DaemonInfo
  repos | add-repo PATH | rm-repo REPO
  worktrees REPO | new-worktree REPO BRANCH [--base REV] [--path P]
  rm-worktree WORKTREE [--force]
  panes
  spawn [--worktree WT] [--cwd DIR] [--env K=V]... [--agent NAME] [--attach] [-- CMD ARGS...]
  launch REPO BRANCH [--base REV] [--agent NAME] [--attach] -- CMD ARGS...
  attach PANE
  kill PANE [--signal hup|term|kill]
  resize PANE COLS ROWS
  scrollback PANE [--before LINE] [--limit N] [--raw]
  status PANE STATUS                    SetAgentStatus
  events                                print events as JSON lines until Ctrl-C
```

- Output: JSON (pretty) for data commands, so tests can parse it.
- `--client-id` default: a random id per run (two clients sharing an id
  kick each other off the daemon; pass one only to resume a specific
  client's state).
- Without `--stdio`, `--ssh` connects with a `FailoverDialer` (QUIC, SSH
  tunnel when UDP is or becomes blocked, back to QUIC when it works).
- `attach`: crossterm raw mode + alternate screen on the local terminal;
  `ResizePane` to the local size on start and on resize events; stdin bytes →
  `ClientSession::send_input`; `SessionEvent::Screen` payloads written to
  stdout as-is (the local terminal plays the role of xterm.js; since the
  local window cannot be resized from the program, the pane is resized to
  the local window instead). Detach with `Ctrl-]`.
  On detach, print a summary to stderr: reconnects, resumes, longest
  `Recovering` duration, bytes in/out.
- `--rebind-every SECS`: a timer calling `ClientSession::notify_network_change()`
  (which rebinds the UDP socket) every SECS seconds; used to prove migration
  works with zero visible interruption. Log each rebind and the time until
  the next received frame (`-v`).

---

## 6. civet-desktop

Tauri 2 app. Rust side in `crates/civet-desktop/src`, React + xterm.js
frontend in `crates/civet-desktop/ui` (Vite, TypeScript, `@xterm/xterm`,
`@xterm/addon-webgl`, `@xterm/addon-fit`). Port sidebar/tabs/split UI ideas
from Orca's `src/renderer` (read-only reference), but keep backend calls
behind one module `ui/src/civet.ts` that wraps `invoke`, `Channel` and
`listen`.

Rust module layout (suggested): `main.rs` (builder, plugins, commands),
`hosts.rs` (HostManager: bootstrap + ClientSession per host), `panes.rs`
(attach → channel pump, batching), `wake.rs` (sleep/wake detector),
`notify.rs` (desktop notifications), `predict.rs` (local echo, phase 2).

### 6.1 Commands (`invoke(name, args)`)

Argument keys are camelCase (Tauri's default). Return values are the proto
JSON forms unless shown. Errors reject the promise with
`{ code: string, message: string }` where `code` is an `ErrorCode` name
(`"notFound"`, …) or one of `"transport"`, `"bootstrap"`, `"noSuchHost"`,
`"daemonBusy"` (an incompatible daemon with live panes is running: offer
`restart_daemon`).

| Command | Args | Returns |
|---|---|---|
| `connect_host` | `{ target: HostTarget, transport?: "auto" \| "quic" \| "ssh" }` | `HostSummary` (resolves once the first connection is up; progress via events) |
| `reconnect_host` | `{ hostId }` | `HostSummary` (connect again with the stored target, incl. ssh port / extra args, and transport choice; `"local"` works without a previous connect) |
| `restart_daemon` | `{ hostId }` | `HostSummary` (re-bootstrap with `replace`: restarts the daemon on this app's version and closes its panes; the UI confirms first) |
| `disconnect_host` | `{ hostId }` | `null` |
| `list_hosts` | `{}` | `HostSummary[]` |
| `notify_network_change` | `{}` | `null` (UI calls it on `window` `online` and `visibilitychange`→visible) |
| `daemon_info` | `{ hostId }` | `DaemonInfo` with `instanceId` as a decimal string |
| `ping` | `{ hostId }` | `{ rttMs: number }` |
| `list_repos` | `{ hostId }` | `RepoView[]` (`RepoInfo` fields plus `kind: "git" \| "folder"`; `ListProjects` from civetd 0.1.17, else `ListRepos` with every `kind: "git"`) |
| `add_repo` | `{ hostId, path }` | `RepoView` (`AddProject` from civetd 0.1.17: a plain folder becomes a folder project; an older daemon refuses it with `invalidArgument` saying to upgrade) |
| `remove_repo` | `{ hostId, repo }` | `null` |
| `list_dirs` | `{ hostId, path }` | `HostDirListing` (subdirectories of any host directory, for the Add project folder field; `unsupported` before civetd 0.1.18) |
| `host_stats` | `{ hostId, since, processes }` | `HostStats` (CPU / memory / swap / network / disk I/O samples newer than `since` (`null`: the daemon's whole history, up to 10 min), cores, load, disks, GPUs, and with `processes` every pane's processes plus the busiest others; `unsupported` before civetd 0.2.1) |
| `list_worktrees` | `{ hostId, repo }` | `WorktreeInfo[]` |
| `create_worktree` | `{ hostId, repo, branch, base?, path? }` | `WorktreeInfo` |
| `remove_worktree` | `{ hostId, worktree, force }` | `null` |
| `list_files` | `{ hostId, worktree, dir, includeIgnored }` | `DirListing` (error `unsupported` before civetd 0.1.10) |
| `read_file` | `{ hostId, worktree, path, maxBytes }` | `FilePreview` |
| `list_all_files` | `{ hostId, worktree }` | `FileList` |
| `file_op` | `{ hostId, worktree, op: FileOp }` (`{ move: { from, to } }`, `{ restore: { trashId } }`…) | `FileOpResult { path, trashId }` (`unsupported` before civetd 0.1.15; `write` is refused here: contents go through `upload_to_worktree`) |
| `upload_to_worktree` | `{ hostId, worktree, dir, paths: string[] }` | `DroppedFile[]` (one per local path, `path` worktree-relative): copies files and folders dropped from Finder into `dir` under their own names (`FileOp::Write` / `CreateDir`; `.git`, symlinks and special files skipped; files over `MAX_UPLOAD_BYTES` fail) |
| `download_file` | `{ hostId, worktree, path, dest }` | bytes written: saves the file at `dest` on this Mac (picked in a save dialog by the UI) in `ReadFileChunk` slices; a failed download removes `dest` |
| `list_panes` | `{ hostId }` | `PaneInfo[]` |
| `spawn_pane` | `{ hostId, request: SpawnPaneRequest }` | `PaneInfo` |
| `launch_agent` | `{ hostId, request: LaunchAgentRequest }` | `{ worktree: WorktreeInfo, pane: PaneInfo }` |
| `attach_pane` | `{ hostId, pane, onOutput: Channel<ArrayBuffer> }` | `SubscribeInfo` (first record on the channel is a Full) |
| `detach_pane` | `{ hostId, pane }` | `null` |
| `write_input` | `{ hostId, pane, data: string }` | `null` (`data` = xterm `onData` string, sent as UTF-8). Seqs follow arrival order: the UI keeps one input invoke per pane in flight |
| `write_input_binary` | `{ hostId, pane, data: string }` | `null` (`onBinary` string: each char code 0–255 is one byte) |
| `resize_pane` | `{ hostId, pane, size: TermSize }` | `null` |
| `kill_pane` | `{ hostId, pane, signal?: "hangup" \| "terminate" \| "kill" }` | `null` |
| `fetch_scrollback` | `{ hostId, pane, query: ScrollbackQuery }` | `ScrollbackPageView` |
| `set_agent_status` | `{ hostId, pane, status: AgentStatus }` | `null` |
| `set_prediction` | `{ enabled: boolean }` | `null` (Settings → "Local echo prediction": Auto = true, Off = false; persisted by the UI and sent at startup) |
| `write_clipboard` | `{ text: string }` | `null` (Cmd+C on an xterm selection; sync command, runs on the main thread for NSPasteboard) |
| `read_clipboard` | `{}` | `string`: the clipboard's plain text, `""` if none (Edit → Paste as File; sync, main thread, like `write_clipboard`; a webview read would prompt) |
| `list_ports` | `{ hostId }` | `PortsEvent` (last report; also pushed as `civet://ports`) |
| `forward_port` | `{ hostId, remotePort, localPort?: number \| null }` | `ForwardView`. Listens on `127.0.0.1:localPort` (null: `remotePort` if free, else any free port; on This Mac never `remotePort`) and tunnels each connection over a `Forward` stream of the host's current session, so forwards survive reconnects and re-bootstraps; `disconnect_host` drops them. Idempotent per remote port (`alreadyExists` if a different `localPort` is asked for, or if `localPort` is taken) |
| `stop_forward` | `{ hostId, remotePort }` | `null` (`notFound` if not forwarded); closes the listener and its connections |
| `list_forwards` | `{ hostId }` | `ForwardView[]` |
| `git_status` | `{ hostId, worktree }` | `GitStatusInfo` (`unsupported` from a daemon older than 0.1.11). The Changes tab polls it every 2.5 s while showing (and so does an open diff tab), every 15 s while the panel shows another tab |
| `git_diff` | `{ hostId, worktree, path, oldPath: string \| null, staged: boolean, contextLines: number }` | `GitDiffInfo` |
| `git_branch` | `{ hostId, worktree }` | `GitBranchInfo` (`unsupported` from a daemon older than 0.1.14: the Changes tab greys its git actions out). Fetched with every `git_status` poll |
| `worktree_lifecycle` | `{ hostId, repo, fetch, review }` | `RepoLifecycle` (`unsupported` before civetd 0.4.1: the sidebar shows no counts and Clean up is unavailable). Fetched after every worktree re-list and once a minute without `fetch` / `review`; the Clean up dialog asks with both |
| `update_worktree` | `{ hostId, worktree, strategy }` | `WorktreeUpdate` (`strategy`: `"rebase"` or `"merge"`) |
| `git_action` | `{ hostId, worktree, action: GitAction }` (serde's external tagging: `{ "stage": { "paths": [...] } }`, `"push"`) | the `GitDiscardSnapshot` of a `discard`, else `null`. After each action the UI fetches the status at once and refetches open diff tabs; a discard's toast offers Undo (`restoreDiscarded`) |
| `open_local_url` | `{ url }` | `null`: opens an `http(s)://localhost` / `127.0.0.1` URL in the default browser (anything else is `invalidArgument`) |

```ts
type HostTarget =
  | { kind: "ssh"; destination: string; port?: number | null; extraArgs?: string[] }
  | { kind: "local" };

type HostSummary = {
  hostId: string;           // "local" or the ssh destination; unique
  label: string;            // destination or "This Mac"
  state: HostState;         // see events
  transport: "quic" | "stream" | null;
  daemonVersion: string | null;
  instanceId: string | null; // u64 as decimal string (JS number would lose precision)
  upgrade: DaemonUpgrade | null;
};

// An older daemon kept because it has live panes although this app's newer
// civetd is installed (BootstrapInfo.upgrade_available).
type DaemonUpgrade = { runningVersion: string; availableVersion: string; livePanes: number };

type PortsEvent = {
  hostId: string;
  status: "pending" | "watching" | "unsupported"; // unsupported: stdio link or a daemon without port detection
  message: string | null;
  ports: ListeningPort[];   // proto JSON
};
type ForwardView = { remotePort: number; localPort: number; connections: number; lastError: string | null };

type ScrollbackPageView = {
  start: number; oldestAvailable: number; endAvailable: number;
  lines: string[];          // VT text per line (UTF-8 decoded)
  wrapped: boolean[];
};
```

Numbers: pane ids, state numbers and line numbers are u64 but stay far below
2^53 in practice; JSON numbers are acceptable for them. `instanceId` is a
string (`HostSummary.instanceId`, `DaemonInfo.instanceId`).

### 6.2 Binary channel framing (`attach_pane` → `onOutput`)

Rust sends `tauri::ipc::InvokeResponseBody::Raw(Vec<u8>)` on a
`tauri::ipc::Channel<InvokeResponseBody>`; JS receives an `ArrayBuffer`.
Each message holds **one or more records** back to back. All integers are
little-endian.

```
offset size field
0      1    kind      1 = screen, 2 = input-ack, 3 = pane-closed, 4 = predict
1      1    flags     bit0 FULL (UpdateKind::Full)
                      bit1 RESIZED (cols/rows differ from previous record)
                      bit2 HISTORY_GAP (history_start > previous history_lines;
                           lines in between are only available via fetch_scrollback)
2      2    cols
4      2    rows
6      2    reserved (0)
8      8    state            (u64; for 4 = predict: the state it overlays)
16     8    ackedInputSeq    (u64)
24     8    historyStart     (u64)
32     8    historyLines     (u64)
40     4    payloadLen       (u32)
44     n    payload          (VT bytes; empty for kinds 2 and 3)
```

Frontend handling per record: if `cols/rows` differ from the xterm size,
`term.resize(cols, rows)` first (the server's PTY size is authoritative;
the UI requests sizes with `resize_pane` from the fit addon, debounced
100 ms). Then `term.write(payload)` (a `Uint8Array` view, no copy). Kind 3:
show "[process exited]" state, stop input. Never transform the payload.

Rust side batching: records for one pane are appended to a buffer and
flushed when 4 ms passed since the first unflushed record, or the buffer
exceeds 256 KiB, whichever comes first (frame-rate batching without adding
visible latency for keystroke echoes).

### 6.3 Events (`listen(name)`)

| Event | Payload |
|---|---|
| `civet://host-state` | `{ hostId, state: HostState, transport: "quic" \| "stream" \| null, rttMs: number \| null, message: string \| null, upgrade: DaemonUpgrade \| null }` |
| `civet://bootstrap-progress` | `{ hostId, step: "probe" \| "upload" \| "start" \| "resolve" \| "dial" \| "fallback", message: string \| null }` |
| `civet://daemon-event` | `{ hostId, event: Event }` (proto JSON, e.g. `{ "paneTitle": { "pane": 3, "title": "vim" } }`) |
| `civet://agent-attention` | `{ hostId, pane, status: "done" \| "needsInput", previous, title, worktree: string \| null, message: string \| null }` |
| `civet://ports` | `PortsEvent`, when a host's listening ports or their status change |
| `civet://forwards` | `{ hostId, forwards: ForwardView[] }` after any change (added, stopped, connection count, `lastError`) |
| `civet://clipboard` | `{ hostId, pane, chars: number, ok: boolean }`: a pane's program set the clipboard (OSC 52). Rust already wrote the system clipboard (NSPasteboard, no user gesture needed); success is silent, the UI shows an error toast when `ok` is false. Applied only while a Civet window is focused and the pane is attached in the UI, at most once per second per pane; otherwise dropped (debug log, no event). `Event::Clipboard` is not forwarded as a `civet://daemon-event` |
| `civet://paste-as-file` | `null`, to the main window: Edit → Paste as File (⇧⌘V, the menu item's key equivalent; the webview never handles the key itself). The focused terminal pane reads `read_clipboard`, uploads the text with `upload_file` as `paste-<time>.txt` and types the path; with no terminal focused the UI shows a short notice |

```ts
type HostState = "connecting" | "connected" | "reconnecting" | "disconnected" | "failed";
```

Mapping from transport `LinkState` (in Rust, `hosts.rs`):

- `Connecting` → `"connecting"` (initial connect only).
- `Connected` → `"connected"` (cancel any pending reconnecting timer).
- `Recovering { since_ms }` → start a 1 s timer; emit `"reconnecting"` only
  if still not `Connected` when it fires. Sub-second resumes are invisible.
- `Failed { needs_bootstrap: true }` → run bootstrap again automatically
  once (progress events); if that fails, `"failed"`. The new session's RTT
  is published again (prediction needs it).
- `Failed { reason: REPLACED_REASON }` → `"failed"` with the message
  "another Civet window or client took over this host's connection;
  reconnect here to take it back".
- `Closed` → `"disconnected"`.

Desktop notifications: on `Event::AgentStatus` with `status` in
{`done`, `needsInput`} and `previous != status`, for an agent pane
(`PaneInfo.agent` set, or the event's source is `hook`), emit
`civet://agent-attention` and, if no app window is focused, show a native
notification via `tauri-plugin-notification` (title = pane title or branch,
body = message or "Agent finished" / "Agent needs input"). At most one
notification per (pane, status) per 10 s. Shell panes never notify.

### 6.4 Wake / network detection (`wake.rs`)

A 1 s `tokio::time::interval` comparing wall-clock (`SystemTime`) delta to
monotonic delta: a jump > 2 s means the machine slept → call
`notify_network_change()` on every host session. The UI also calls the
`notify_network_change` command on `online` / visibility events. (Optional
later: macOS `NSWorkspaceDidWakeNotification`.)

### 6.5 Transport choice

`transport: "auto"` (default): bootstrap over SSH, then connect with a
`FailoverDialer` (QUIC with a 3 s connect timeout, `StreamDialer` over
`civetd::bootstrap::open_ssh_tunnel` as the fallback). If the first
connection ends up on the tunnel, emit `bootstrap-progress` step
`"fallback"`. The failover dialer also moves the session to the tunnel
when UDP gets blocked mid-session and back to QUIC when it works again
(probe every 10 s), so "auto" is kept for re-bootstraps. `"quic"` = QUIC
only, `"ssh"` = tunnel only. The desktop's client id is persisted in
`~/.civet/client-id-desktop` (not shared with civet-cli).

Daemon binaries: the packaged app bundles `civetd-<triple>` files in
`Civet.app/Contents/Resources/civet-dist/` (x86_64 / aarch64 Linux musl for
remotes, this Mac's triple for the Local host), built by
`scripts/build-remote.sh --bundle` as `tauri build`'s `beforeBuildCommand`.
Upload search order: the bundled dir, `$CIVET_DIST_DIR`, `~/.civet/dist`,
the `civetd` next to the executable (`BinarySource::with_bundled`). Local:
`$CIVET_CIVETD`, the bundled `civetd-<this triple>` (version-checked), the
`civetd` next to the executable (dev: the workspace target dir), `$PATH`,
`~/.civet/bin/civetd`. Dev builds (`tauri dev`) bundle nothing.

Daemon upgrades: when the bootstrap keeps an older daemon with live panes,
`HostSummary.upgrade` / `host-state.upgrade` are set; the UI shows the
upgrade (banner above the terminal, host popover, sidebar, one toast) with
"Restart daemon (closes K panes)" → confirm → `restart_daemon`.

### 6.6 Local-echo prediction (phase 2, week 5+)

Lives in Rust: `civet-desktop/src/predict.rs`, one `Predictor` per attached
pane. It keeps a **client-side mirror** `civet_term::PaneTerminal` fed with
every server payload, so it knows the true screen at `current_state`.

- On `write_input` of printable text (and Backspace / Left / Right) while the
  cursor is on the primary screen, not in a mode that makes echo unlikely
  (alt screen with no recent echo, password-like prompts where the
  previous keystrokes were not echoed), predict the new cell contents and
  cursor; immediately send a kind-4 record whose payload draws the
  predictions underlined (SGR 4) at the predicted cells and moves the cursor.
- Before forwarding any server screen record, prepend undo bytes:
  `civet_term::encode_cells(mirror_snapshot, row, cols)` for every predicted
  cell (restores the true `from` state the diff expects), then the server
  payload, then re-draw still-pending predictions. Confirmed predictions
  (server cell matches and `ackedInputSeq` covers the input) are retired;
  mispredictions are dropped and prediction pauses for that pane until the
  next RTT-ish quiet period (mosh's behavior).
- Show predictions only when RTT > 30 ms (mosh "adaptive" mode).
- The frontend does nothing special: it writes payloads.

---

## 7. On-disk layout and environment

Remote/host side (`~/.civet`, or `$CIVET_STATE_DIR`):

| Path | Purpose |
|---|---|
| `bin/civetd-<version>`, `bin/civetd` (symlink) | installed daemon binaries |
| `civetd.lock` | single-instance flock |
| `daemon.json` (0600) | running daemon's `BootstrapInfo`; written last when ready |
| `civetd.sock` (0600) | unix socket: stdio-tunnel and local clients (same protocol as QUIC control stream) |
| `cert.der`, `key.der` (0600) | daemon TLS identity |
| `repos.json` | registered projects `[{ "id", "name", "path", "kind" }]` (`kind` `"git"` or `"folder"`; missing in older files = `"git"`) |
| `status/<pane>.status` | per-pane hook file (`CIVET_STATUS_FILE`) |
| `trash/<id>/` | files deleted from the desktop's Files tab (`item` + `meta.json`), restorable with `FileOp::Restore`; purged after 3 days / beyond 200 entries |
| `hooks/claude-settings.json` | generated Claude Code hook settings |
| `hooks/civet-status-hook.sh` | status hook helper |
| `hooks/bin/claude`, `hooks/bin/codex` | PATH shims adding Civet's hooks (first on every pane's PATH) |
| `hooks/bin/civet` | runs this daemon's `civetd` against this state dir (`civet handoff`) |
| `hooks/zsh/.zshenv` | zsh `ZDOTDIR` shim for panes |
| `logs/civetd.log` | daemon log when detached |

Client side (`~/.civet` on the laptop): `client-id-desktop` (hex, the
desktop's `ClientId`), `client-id` (legacy, unused by the shipped clients), `ssh/` (ssh
ControlPath sockets, 0700), `dist/` (optional `civetd-<triple>` binaries for
dev; also `CIVET_DIST_DIR`).

| Env var | Where | Meaning |
|---|---|---|
| `CIVET_STATE_DIR` | daemon, cli | state dir override |
| `CIVET_PORT_RANGE` | daemon | e.g. `60001-60999` |
| `CIVET_LOG` | all | tracing env-filter (default `info`) |
| `CIVET_DIST_DIR` | cli, desktop | directory of `civetd-<triple>` binaries |
| `CIVET_PANE_ID` | panes | pane id |
| `CIVET_STATUS_FILE` | panes | hook file path |
| `CIVET_WORKTREE` | panes | worktree path (if any) |
| `CIVET_SHIM_DIR` | panes | agent shim dir (first on `PATH`) |
| `CIVET_USER_ZDOTDIR` | zsh panes | the user's `ZDOTDIR`, restored by the shim |
| `TERM_PROGRAM` | panes | `civet` |

---

## 8. Constants cheat sheet

| Constant | Value | Defined in |
|---|---|---|
| Protocol version | 1 | `civet_proto::PROTOCOL_VERSION` |
| ALPN | `civet/1` | `civet_proto::ALPN` |
| QUIC idle timeout | 6 h | transport |
| Client keep-alive | 2 s | transport |
| Heartbeat / stall | 1 s / 1 s | `ClientSessionConfig` |
| Parallel re-dial after | 3 s | `ClientSessionConfig` |
| UI "reconnecting" threshold | 1 s | desktop |
| Resume TTL on daemon | 24 h | civetd |
| Max in-flight updates per pane | 8 | `civet_proto::MAX_INFLIGHT_UPDATES` |
| Update coalescing | 2 ms after idle, ≤ 1 per 8 ms | civet-session |
| Echo-ack delay | 50 ms | `civet_proto::ECHO_ACK_DELAY_MS` |
| History replay per Diff / Full | 2000 / 1000 lines, ≤ 8 MiB | `civet_proto::{MAX_REPLAY_LINES, FULL_REPLAY_LINES}`, `civet_term::MAX_REPLAY_BYTES` |
| Max screen-update payload | 12 MiB | `civet_term::MAX_UPDATE_BYTES` |
| Max pane size | 1000 cols x 500 rows | `civet_proto::{MAX_TERM_COLS, MAX_TERM_ROWS}` |
| Snapshot ring per pane | 2 | `civet_term::DEFAULT_SNAPSHOT_RING` |
| Scrollback retained per pane | 10 000 lines | `TermConfig` / `SessionConfig` |
| UDP port range | 60001–60999 | civetd |
| Pre-auth frame limits (StreamOpen / hello) | 64 B / 64 KiB | `civet_proto::{MAX_STREAM_OPEN_LEN, MAX_HELLO_LEN}` |
| Concurrent unauthenticated handshakes | 32 per listener | `civetd::daemon::MAX_PENDING_HANDSHAKES` |
| "Replaced" retry delay / give-up window | 1 s / 30 s | `civet_transport::{REPLACED_RETRY_DELAY, REPLACED_WINDOW}` |
| Unacked input retransmit | after 1 s without ack progress, ≤ 64 KiB | `civet_transport::session::INPUT_RETRANSMIT_AFTER` |
| Failover: primary timeout / after / probe | 3 s (2 attempts) / 3 s / 10 s | `civet_transport::FailoverConfig` |
| Desktop channel flush | 4 ms or 256 KiB | desktop |
