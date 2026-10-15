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

export type HostSummary = {
  hostId: string
  label: string
  state: HostState
  transport: HostTransport | null
  daemonVersion: string | null
  instanceId: string | null
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
  instanceId: number
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

/** Promise rejection shape of every command (INTERFACES §6.1). */
export type CivetErrorPayload = { code: string; message: string }

export function termSize(cols: number, rows: number): TermSize {
  return { cols, rows, pixelWidth: 0, pixelHeight: 0 }
}
