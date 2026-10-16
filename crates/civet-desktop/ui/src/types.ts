// JSON shapes exchanged with the Rust side of civet-desktop.
// Source of truth: docs/INTERFACES.md §1 (proto JSON form: serde external
// tagging, camelCase) and §6 (desktop commands/events). Keep in sync.

export type PaneId = number
export type RepoId = string
/** Canonical worktree path. */
export type WorktreeId = string

export type HostState = 'connecting' | 'connected' | 'reconnecting' | 'disconnected' | 'failed'
export type HostTransport = 'quic' | 'stream'
export type TransportChoice = 'auto' | 'quic' | 'ssh'

export type HostTarget =
  | { kind: 'ssh'; destination: string; port?: number | null; extraArgs?: string[] }
  | { kind: 'local' }

/**
 * An older civetd kept running because it has live panes, although this app's
 * newer one is installed. `restart_daemon` upgrades it (closing the panes).
 */
export type DaemonUpgrade = {
  runningVersion: string
  availableVersion: string
  /** Live panes when the host was bootstrapped. */
  livePanes: number
}

export type HostSummary = {
  hostId: string
  label: string
  state: HostState
  transport: HostTransport | null
  daemonVersion: string | null
  instanceId: string | null
  upgrade: DaemonUpgrade | null
}

export type TermSize = {
  cols: number
  rows: number
  pixelWidth: number
  pixelHeight: number
}

export type AgentStatus = 'unknown' | 'running' | 'needsInput' | 'done' | 'idle'
export type AgentStatusSource = 'escapeCode' | 'hook' | 'heuristic' | 'processExit' | 'client'
export type KillSignal = 'hangup' | 'terminate' | 'kill'

/**
 * A project is a git repository (worktrees, sessions, changes) or a plain
 * folder: one worktree (its root) with file tabs only.
 */
export type ProjectKind = 'git' | 'folder'

export type RepoInfo = {
  id: RepoId
  name: string
  path: string
  defaultBranch: string | null
  /** Missing from backends that predate folder projects: a repository. */
  kind?: ProjectKind
}

export type WorktreeInfo = {
  id: WorktreeId
  repo: RepoId
  path: string
  branch: string | null
  head: string | null
  isMain: boolean
  locked: boolean
}

export type PullRequestState = 'open' | 'draft' | 'merged' | 'closed'

export type PullRequestInfo = {
  number: number
  branch: string
  title: string
  url: string
  state: PullRequestState
}

export type FileKind = 'file' | 'dir' | 'symlink'

export type FileEntry = {
  name: string
  kind: FileKind
  /** Bytes (`lstat`); 0 for directories. */
  size: number
  /** Gitignored (only listed with `includeIgnored`). */
  ignored: boolean
}

/** One directory level: directories first, then by name. */
export type DirListing = { entries: FileEntry[]; truncated: boolean }

/** A subdirectory of a host directory (`list_dirs`, civetd >= 0.1.18). */
export type HostDir = {
  name: string
  /** Has `.git` (repository or worktree root) or is a bare repository. */
  gitRepo: boolean
}

/** Subdirectories of any host directory, for completing a folder path. */
export type HostDirListing = {
  /** The directory listed: absolute, `~` expanded. */
  path: string
  /** By name (case-insensitive), hidden ones included. */
  dirs: HostDir[]
  truncated: boolean
}

export type FilePreview = {
  /** Size of the whole file. */
  size: number
  /** NUL in the first 8 KiB: `text` is empty. */
  binary: boolean
  truncated: boolean
  text: string
}

/** Worktree-relative paths (`/`-separated), sorted. */
export type FileList = { paths: string[]; truncated: boolean }

/**
 * A change to worktree files (`file_op`, civetd >= 0.1.15). Paths are
 * relative to the worktree root; nothing is overwritten (`alreadyExists`)
 * and `.git` is off limits. File contents go through `uploadToWorktree`.
 */
export type FileOp =
  | { createFile: { path: string } }
  | { createDir: { path: string } }
  | { move: { from: string; to: string } }
  | { copy: { from: string; to: string } }
  /** To the daemon's trash; `trashId` of the result restores it. */
  | { trash: { path: string } }
  | { restore: { trashId: string } }

export type FileOpResult = {
  /** The path created or changed (for `restore`: where the entry is back). */
  path: string
  trashId: string | null
}

export type GitChangeKind =
  | 'modified'
  | 'added'
  | 'deleted'
  | 'renamed'
  | 'copied'
  | 'typeChanged'
  | 'untracked'
  | 'conflicted'

export type GitFileChange = {
  /** Relative to the worktree root. */
  path: string
  /** Source of a rename / copy. */
  oldPath: string | null
  kind: GitChangeKind
  /** null: binary or not counted. */
  additions: number | null
  deletions: number | null
}

export type GitStatusInfo = {
  staged: GitFileChange[]
  unstaged: GitFileChange[]
  untracked: GitFileChange[]
  conflicted: GitFileChange[]
  /** Entries past the daemon's cap were left out. */
  truncated: boolean
}

export type GitDiffInfo = {
  path: string
  binary: boolean
  tooLarge: boolean
  additions: number | null
  deletions: number | null
  /** `git diff` output (headers + hunks); empty when binary / tooLarge / unchanged. */
  diff: string
}

/** A worktree's branch vs. its upstream (`git_branch`, civetd >= 0.1.14). */
export type GitBranchInfo = {
  /** null: detached HEAD. */
  branch: string | null
  /** HEAD commit; null on an unborn branch. */
  head: string | null
  /** `origin/feat`; null without one or before the first push. */
  upstream: string | null
  ahead: number
  behind: number
  /** HEAD has a parent and is on no remote branch. */
  canUndoCommit: boolean
  /** Stash entries made on this branch. */
  stashes: number
  hasRemote: boolean
}

/** A blob in the object database and the git mode to restore it with. */
export type GitBlobRef = { oid: string; mode: number }

export type GitDiscardEntry = {
  path: string
  /** null: the file did not exist. */
  file: GitBlobRef | null
  restoreIndex: boolean
  index: GitBlobRef | null
  untracked: boolean
}

/** What a discard overwrote; send it back in `restoreDiscarded` to undo. */
export type GitDiscardSnapshot = { entries: GitDiscardEntry[] }

export type ConflictSide = 'ours' | 'theirs'

/** `git_action` (civet_proto::GitAction, serde's externally tagged JSON). */
export type GitAction =
  | { stage: { paths: string[] } }
  | { unstage: { paths: string[] } }
  | { discard: { paths: string[]; staged: boolean } }
  | { restoreDiscarded: GitDiscardSnapshot }
  | { commit: { message: string; amend: boolean; all: boolean } }
  | { acceptConflict: { paths: string[]; side: ConflictSide } }
  | 'push'
  | 'pull'
  | 'fetch'
  | { undoCommit: { head: string } }
  | 'stashAll'
  | 'stashPop'

export type CommandSpec = { program: string; args: string[] }

export type SpawnPaneRequest = {
  worktree: WorktreeId | null
  cwd: string | null
  command: CommandSpec | null
  env: [string, string][]
  size: TermSize
  title: string | null
  agent: string | null
}

export type LaunchAgentRequest = {
  repo: RepoId
  branch: string
  base: string | null
  worktreePath: string | null
  command: CommandSpec
  env: [string, string][]
  size: TermSize
  title: string | null
  agent: string | null
}

export type PaneInfo = {
  id: PaneId
  worktree: WorktreeId | null
  cwd: string
  command: string[]
  title: string
  size: TermSize
  agent: string | null
  status: AgentStatus
  exitCode: number | null
  exited: boolean
  createdAtMs: number
  state: number
  /**
   * UI only (not sent by civetd): the first prompt sent to the agent, from
   * its UserPromptSubmit hook. Labels the session until the agent sets a
   * title of its own.
   */
  prompt?: string
}

export type SubscribeInfo = {
  pane: PaneId
  state: number
  size: TermSize
  historyLines: number
  lastInputSeq: number
}

export type DaemonInfo = {
  daemonVersion: string
  protoVersion: number
  pid: number
  /** u64 as a decimal string. */
  instanceId: string
  startedAtMs: number
  hostname: string
}

export type ScrollbackQuery =
  | { before: { end: number | null; limit: number } }
  | { range: { start: number; end: number } }

export type ScrollbackPageView = {
  start: number
  oldestAvailable: number
  endAvailable: number
  lines: string[]
  wrapped: boolean[]
}

export type AgentStatusEvent = {
  pane: PaneId
  status: AgentStatus
  previous: AgentStatus
  source: AgentStatusSource
  message: string | null
  atMs: number
}

/** Proto `Event`, serde external tagging. */
export type DaemonEvent =
  | { paneCreated: PaneInfo }
  | { paneExited: { pane: PaneId; exitCode: number | null } }
  | { paneClosed: { pane: PaneId } }
  | { paneTitle: { pane: PaneId; title: string } }
  | { paneResized: { pane: PaneId; size: TermSize } }
  | { agentStatus: AgentStatusEvent }
  | { bell: { pane: PaneId } }
  | 'reposChanged'
  | { worktreesChanged: { repo: RepoId } }

export type HostStateEvent = {
  hostId: string
  state: HostState
  transport: HostTransport | null
  rttMs: number | null
  message: string | null
  upgrade: DaemonUpgrade | null
}

export type BootstrapStep = 'probe' | 'upload' | 'start' | 'resolve' | 'dial' | 'fallback'

export type BootstrapProgressEvent = {
  hostId: string
  step: BootstrapStep
  message: string | null
}

export type DaemonEventEnvelope = { hostId: string; event: DaemonEvent }

/** `civet://clipboard`: a pane's program set the clipboard (OSC 52); Rust already wrote it. */
export type ClipboardEvent = { hostId: string; pane: PaneId; chars: number; ok: boolean }

export type AgentAttentionEvent = {
  hostId: string
  pane: PaneId
  status: 'done' | 'needsInput'
  previous: AgentStatus
  title: string
  worktree: string | null
  message: string | null
}

/** A TCP port something listens on, on a host (`civet_proto::ListeningPort`). */
export type ListeningPort = {
  port: number
  /** Bound addresses: `127.0.0.1`, `::1`, `0.0.0.0`, `::`... */
  addrs: string[]
  pid: number | null
  /** Process name (`node`, `python3`...). */
  process: string | null
  /** Command line (truncated). */
  command: string | null
  /** Pane whose process tree opened it. */
  pane: PaneId | null
  /** The pane's worktree, else the one containing the process's cwd. */
  worktree: WorktreeId | null
}

/** `pending`: not known yet; `unsupported`: SSH tunnel link or an older daemon (see `message`). */
export type PortsStatus = 'pending' | 'watching' | 'unsupported'

/** `civet://ports` and `list_ports`: a host's listening ports. */
export type PortsEvent = {
  hostId: string
  status: PortsStatus
  message: string | null
  ports: ListeningPort[]
}

/** A forwarded port: `localhost:localPort` on this Mac → `remotePort` on the host. */
export type ForwardView = {
  remotePort: number
  localPort: number
  /** Connections open through it right now. */
  connections: number
  /** Why the last connection could not be forwarded; cleared by the next that works. */
  lastError: string | null
}

/** `civet://forwards`: every forward of a host, after any change. */
export type ForwardsEvent = { hostId: string; forwards: ForwardView[] }

/** Promise rejection shape of every command (INTERFACES §6.1). */
export type CivetErrorPayload = { code: string; message: string }

export function termSize(cols: number, rows: number): TermSize {
  return { cols, rows, pixelWidth: 0, pixelHeight: 0 }
}
