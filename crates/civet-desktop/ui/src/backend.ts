// Thin typed client for the civet-desktop Rust core (docs/INTERFACES.md §6).
//
// Every backend call in the UI goes through the `CivetBackend` interface.
// Inside Tauri it is `TauriBackend` (invoke / Channel / listen, exactly per
// the contract); in a plain browser (`pnpm dev`) it is the mock backend in
// backend.mock.ts, so the UI is usable without the Rust side.

import { Channel, invoke, isTauri } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { toBytes } from './terminal/records'
import type {
  AgentAttentionEvent,
  AgentStatus,
  BootstrapProgressEvent,
  CivetErrorPayload,
  DaemonEventEnvelope,
  DaemonInfo,
  HostStateEvent,
  HostSummary,
  HostTarget,
  KillSignal,
  LaunchAgentRequest,
  PaneId,
  PaneInfo,
  RepoId,
  RepoInfo,
  ScrollbackPageView,
  ScrollbackQuery,
  SpawnPaneRequest,
  SubscribeInfo,
  TermSize,
  TransportChoice,
  WorktreeId,
  WorktreeInfo
} from './types'

export type Unlisten = () => void

/** Event names (INTERFACES §6.3). */
export const CivetEvents = {
  hostState: 'civet://host-state',
  bootstrapProgress: 'civet://bootstrap-progress',
  daemonEvent: 'civet://daemon-event',
  agentAttention: 'civet://agent-attention'
} as const

/** Rejection type of every backend call: `{ code, message }` from Rust, as an Error. */
export class CivetError extends Error {
  constructor(
    readonly code: string,
    message: string
  ) {
    super(message)
    this.name = 'CivetError'
  }

  static from(err: unknown): CivetError {
    if (err instanceof CivetError) return err
    if (err && typeof err === 'object' && 'code' in err && 'message' in err) {
      const e = err as CivetErrorPayload
      return new CivetError(String(e.code), String(e.message))
    }
    if (typeof err === 'string') return new CivetError('unknown', err)
    if (err instanceof Error) return new CivetError('unknown', err.message)
    return new CivetError('unknown', String(err))
  }
}

/** Receives raw channel messages (one or more records) for an attached pane. */
export type PaneOutputHandler = (message: Uint8Array) => void

export type PaneAttachment = {
  info: SubscribeInfo
  /** Stop delivering output to the handler and `detach_pane`. */
  detach(): Promise<void>
}

export interface CivetBackend {
  readonly kind: 'tauri' | 'mock'

  connectHost(target: HostTarget, transport?: TransportChoice): Promise<HostSummary>
  disconnectHost(hostId: string): Promise<void>
  listHosts(): Promise<HostSummary[]>
  notifyNetworkChange(): Promise<void>
  daemonInfo(hostId: string): Promise<DaemonInfo>
  ping(hostId: string): Promise<{ rttMs: number }>

  listRepos(hostId: string): Promise<RepoInfo[]>
  addRepo(hostId: string, path: string): Promise<RepoInfo>
  removeRepo(hostId: string, repo: RepoId): Promise<void>
  listWorktrees(hostId: string, repo: RepoId): Promise<WorktreeInfo[]>
  createWorktree(
    hostId: string,
    repo: RepoId,
    branch: string,
    base?: string | null,
    path?: string | null
  ): Promise<WorktreeInfo>
  removeWorktree(hostId: string, worktree: WorktreeId, force: boolean): Promise<void>

  listPanes(hostId: string): Promise<PaneInfo[]>
  spawnPane(hostId: string, request: SpawnPaneRequest): Promise<PaneInfo>
  launchAgent(
    hostId: string,
    request: LaunchAgentRequest
  ): Promise<{ worktree: WorktreeInfo; pane: PaneInfo }>
  attachPane(hostId: string, pane: PaneId, onOutput: PaneOutputHandler): Promise<PaneAttachment>
  writeInput(hostId: string, pane: PaneId, data: string): Promise<void>
  writeInputBinary(hostId: string, pane: PaneId, data: string): Promise<void>
  resizePane(hostId: string, pane: PaneId, size: TermSize): Promise<void>
  killPane(hostId: string, pane: PaneId, signal?: KillSignal): Promise<void>
  fetchScrollback(hostId: string, pane: PaneId, query: ScrollbackQuery): Promise<ScrollbackPageView>
  setAgentStatus(hostId: string, pane: PaneId, status: AgentStatus): Promise<void>
  setPrediction(enabled: boolean): Promise<void>

  onHostState(cb: (e: HostStateEvent) => void): Promise<Unlisten>
  onBootstrapProgress(cb: (e: BootstrapProgressEvent) => void): Promise<Unlisten>
  onDaemonEvent(cb: (e: DaemonEventEnvelope) => void): Promise<Unlisten>
  onAgentAttention(cb: (e: AgentAttentionEvent) => void): Promise<Unlisten>
}

type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>
type ListenFn = <T>(event: string, cb: (e: { payload: T }) => void) => Promise<UnlistenFn>
type ChannelCtor = new <T>() => Channel<T>

export type TauriDeps = { invoke: InvokeFn; listen: ListenFn; Channel: ChannelCtor }

const defaultDeps: TauriDeps = {
  invoke: invoke as InvokeFn,
  listen: listen as ListenFn,
  Channel: Channel as ChannelCtor
}

export class TauriBackend implements CivetBackend {
  readonly kind = 'tauri' as const
  /**
   * In-flight detach per pane. A re-mount (tab switch, split re-parent) can
   * detach and re-attach the same pane back to back; Tauri runs async
   * commands concurrently, so the attach waits for the detach to land first.
   */
  private detaching = new Map<string, Promise<void>>()

  constructor(private readonly deps: TauriDeps = defaultDeps) {}

  private async call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
    try {
      return await this.deps.invoke<T>(cmd, args)
    } catch (err) {
      throw CivetError.from(err)
    }
  }

  private async callVoid(cmd: string, args: Record<string, unknown> = {}): Promise<void> {
    await this.call<null>(cmd, args)
  }

  connectHost(target: HostTarget, transport: TransportChoice = 'auto') {
    return this.call<HostSummary>('connect_host', { target, transport })
  }
  disconnectHost(hostId: string) {
    return this.callVoid('disconnect_host', { hostId })
  }
  listHosts() {
    return this.call<HostSummary[]>('list_hosts')
  }
  notifyNetworkChange() {
    return this.callVoid('notify_network_change')
  }
  daemonInfo(hostId: string) {
    return this.call<DaemonInfo>('daemon_info', { hostId })
  }
  ping(hostId: string) {
    return this.call<{ rttMs: number }>('ping', { hostId })
  }
  listRepos(hostId: string) {
    return this.call<RepoInfo[]>('list_repos', { hostId })
  }
  addRepo(hostId: string, path: string) {
    return this.call<RepoInfo>('add_repo', { hostId, path })
  }
  removeRepo(hostId: string, repo: RepoId) {
    return this.callVoid('remove_repo', { hostId, repo })
  }
  listWorktrees(hostId: string, repo: RepoId) {
    return this.call<WorktreeInfo[]>('list_worktrees', { hostId, repo })
  }
  createWorktree(
    hostId: string,
    repo: RepoId,
    branch: string,
    base: string | null = null,
    path: string | null = null
  ) {
    return this.call<WorktreeInfo>('create_worktree', { hostId, repo, branch, base, path })
  }
  removeWorktree(hostId: string, worktree: WorktreeId, force: boolean) {
    return this.callVoid('remove_worktree', { hostId, worktree, force })
  }
  listPanes(hostId: string) {
    return this.call<PaneInfo[]>('list_panes', { hostId })
  }
  spawnPane(hostId: string, request: SpawnPaneRequest) {
    return this.call<PaneInfo>('spawn_pane', { hostId, request })
  }
  launchAgent(hostId: string, request: LaunchAgentRequest) {
    return this.call<{ worktree: WorktreeInfo; pane: PaneInfo }>('launch_agent', {
      hostId,
      request
    })
  }

  async attachPane(hostId: string, pane: PaneId, onOutput: PaneOutputHandler) {
    const key = `${hostId}\u0000${pane}`
    await this.detaching.get(key)?.catch(() => {})
    let active = true
    const channel = new this.deps.Channel<ArrayBuffer>()
    // Set the handler before invoking: the first (Full) record may arrive
    // before `attach_pane` resolves, and unhandled messages are not replayed.
    channel.onmessage = (message: unknown) => {
      if (!active) return
      let bytes: Uint8Array
      try {
        bytes = toBytes(message)
      } catch (err) {
        console.error('[civet] dropping channel message', err)
        return
      }
      onOutput(bytes)
    }
    let info: SubscribeInfo
    try {
      info = await this.call<SubscribeInfo>('attach_pane', { hostId, pane, onOutput: channel })
    } catch (err) {
      active = false
      throw err
    }
    return {
      info,
      detach: async () => {
        if (!active) return
        active = false
        channel.onmessage = () => {}
        const p = this.callVoid('detach_pane', { hostId, pane })
        this.detaching.set(key, p)
        try {
          await p
        } finally {
          if (this.detaching.get(key) === p) this.detaching.delete(key)
        }
      }
    }
  }

  writeInput(hostId: string, pane: PaneId, data: string) {
    return this.callVoid('write_input', { hostId, pane, data })
  }
  writeInputBinary(hostId: string, pane: PaneId, data: string) {
    return this.callVoid('write_input_binary', { hostId, pane, data })
  }
  resizePane(hostId: string, pane: PaneId, size: TermSize) {
    return this.callVoid('resize_pane', { hostId, pane, size })
  }
  killPane(hostId: string, pane: PaneId, signal?: KillSignal) {
    return this.callVoid('kill_pane', signal ? { hostId, pane, signal } : { hostId, pane })
  }
  fetchScrollback(hostId: string, pane: PaneId, query: ScrollbackQuery) {
    return this.call<ScrollbackPageView>('fetch_scrollback', { hostId, pane, query })
  }
  setAgentStatus(hostId: string, pane: PaneId, status: AgentStatus) {
    return this.callVoid('set_agent_status', { hostId, pane, status })
  }
  setPrediction(enabled: boolean) {
    return this.callVoid('set_prediction', { enabled })
  }

  private on<T>(event: string, cb: (payload: T) => void): Promise<Unlisten> {
    return this.deps.listen<T>(event, (e) => cb(e.payload))
  }
  onHostState(cb: (e: HostStateEvent) => void) {
    return this.on(CivetEvents.hostState, cb)
  }
  onBootstrapProgress(cb: (e: BootstrapProgressEvent) => void) {
    return this.on(CivetEvents.bootstrapProgress, cb)
  }
  onDaemonEvent(cb: (e: DaemonEventEnvelope) => void) {
    return this.on(CivetEvents.daemonEvent, cb)
  }
  onAgentAttention(cb: (e: AgentAttentionEvent) => void) {
    return this.on(CivetEvents.agentAttention, cb)
  }
}

/** True inside the Tauri webview. `?mock` or VITE_CIVET_MOCK=1 forces the mock backend. */
export function shouldUseMock(): boolean {
  if (import.meta.env?.VITE_CIVET_MOCK === '1') return true
  if (typeof location !== 'undefined' && new URLSearchParams(location.search).has('mock')) {
    return true
  }
  return !isTauri()
}

let backendPromise: Promise<CivetBackend> | null = null

/** The process-wide backend; the mock is loaded lazily so it stays out of the Tauri path. */
export function getBackend(): Promise<CivetBackend> {
  backendPromise ??= shouldUseMock()
    ? import('./backend.mock').then((m) => new m.MockBackend())
    : Promise.resolve(new TauriBackend())
  return backendPromise
}
