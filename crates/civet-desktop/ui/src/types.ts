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

export type RepoInfo = {
  id: RepoId
  name: string
  path: string
  defaultBranch: string | null
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
