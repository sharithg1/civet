// Thin typed client for the civet-desktop Rust core (docs/INTERFACES.md §6).
//
// Every backend call in the UI goes through the `CivetBackend` interface.
// Inside Tauri it is `TauriBackend` (invoke / Channel / listen, exactly per
// the contract); in a plain browser (`pnpm dev`) it is the mock backend in
// backend.mock.ts, so the UI is usable without the Rust side.

import { Channel, invoke, isTauri } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { toBytes } from './terminal/records'
import type {
  AgentAttentionEvent,
  AgentStatus,
  BootstrapProgressEvent,
  CivetErrorPayload,
  ClipboardEvent,
  DaemonEventEnvelope,
  DaemonInfo,
  ForwardsEvent,
  ForwardView,
  HostStateEvent,
  HostSummary,
  HostTarget,
  KillSignal,
  LaunchAgentRequest,
  PaneId,
  PaneInfo,
  PortsEvent,
  RepoId,
  RepoInfo,
  ScrollbackPageView,
  ScrollbackQuery,
  SpawnPaneRequest,
  SubscribeInfo,
  TermSize,
  TransportChoice,
  WorktreeId,
  WorktreeInfo,
  PullRequestDetail,
  PullRequestInfo,
  CreatePullRequest,
  DirListing,
  HostDirListing,
  FileList,
  FileOp,
  FileOpResult,
  FilePreview,
  GitStatusInfo,
  GitDiffInfo,
  GitBranchInfo,
  GitAction,
  GitDiscardSnapshot,
  HostStats,
  RepoLifecycle,
  UpdateStrategy,
  WorktreeUpdate,
  WorktreeTemplate,
  WorktreeTemplateInfo,
  WorktreeSetup,
  PromptTrigger,
  QueuedPrompt,
  AgentUsage
} from './types'

export type Unlisten = () => void

/** Event names (INTERFACES §6.3). */
export const CivetEvents = {
  hostState: 'civet://host-state',
  bootstrapProgress: 'civet://bootstrap-progress',
  daemonEvent: 'civet://daemon-event',
  agentAttention: 'civet://agent-attention',
  clipboard: 'civet://clipboard',
  pasteAsFile: 'civet://paste-as-file',
  ports: 'civet://ports',
  forwards: 'civet://forwards'
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
  /** Connect again with the host's stored target (ssh port/args) and transport choice. */
  reconnectHost(hostId: string): Promise<HostSummary>
  /** Restart the host's daemon on this app's version (closes its panes). */
  restartDaemon(hostId: string): Promise<HostSummary>
  disconnectHost(hostId: string): Promise<void>
  listHosts(): Promise<HostSummary[]>
  notifyNetworkChange(): Promise<void>
  daemonInfo(hostId: string): Promise<DaemonInfo>
  ping(hostId: string): Promise<{ rttMs: number }>

  listRepos(hostId: string): Promise<RepoInfo[]>
  addRepo(hostId: string, path: string): Promise<RepoInfo>
  removeRepo(hostId: string, repo: RepoId): Promise<void>
  /**
   * Subdirectories of any host directory (absolute or `~/…`), git
   * repositories flagged, for completing a folder path (civetd >= 0.1.18).
   */
  listDirs(hostId: string, path: string): Promise<HostDirListing>
  /**
   * The host's CPU / memory / disk / network / GPU use: samples newer than
   * `since` (all kept without it), plus processes by pane if `processes`
   * (civetd >= 0.2.1).
   */
  hostStats(hostId: string, since: number | null, processes: boolean): Promise<HostStats>
  /**
   * Token use and estimated spend of the host's Claude Code / Codex sessions
   * since `sinceMs`, from their own logs (civetd >= 0.4.5).
   */
  agentUsage(hostId: string, sinceMs: number): Promise<AgentUsage>
  listWorktrees(hostId: string, repo: RepoId): Promise<WorktreeInfo[]>
  /** A repo's worktree template and its sources (civetd >= 0.4.2). */
  worktreeTemplate(hostId: string, repo: RepoId): Promise<WorktreeTemplateInfo>
  /** Replace the template saved on the host (all null: delete it). */
  setWorktreeTemplate(
    hostId: string,
    repo: RepoId,
    template: WorktreeTemplate
  ): Promise<WorktreeTemplateInfo>
  /**
   * Apply the template to a new worktree: copy files from the main checkout,
   * assign its `CIVET_PORT`, start the setup script in a pane. The caller
   * starts the agent.
   */
  setupWorktree(hostId: string, worktree: WorktreeId, size: TermSize): Promise<WorktreeSetup>
  /** `gh pr list` on the host, newest first (needs civetd >= 0.1.5). */
  listPullRequests(hostId: string, repo: RepoId): Promise<PullRequestInfo[]>
  /**
   * The newest pull request of the worktree's branch with CI checks and
   * reviews, null if none (needs civetd >= 0.4.3). `threads`: also the
   * unresolved review threads.
   */
  pullRequestDetail(
    hostId: string,
    worktree: WorktreeId,
    threads: boolean
  ): Promise<PullRequestDetail | null>
  /** `gh pr create` on the host for the worktree's (pushed) branch (needs civetd >= 0.4.3). */
  createPullRequest(hostId: string, request: CreatePullRequest): Promise<PullRequestInfo>
  /** `git status` of a worktree with line counts (needs civetd >= 0.1.11). */
  gitStatus(hostId: string, worktree: WorktreeId): Promise<GitStatusInfo>
  /** One file's diff: index vs. HEAD if `staged`, else working tree vs. index. */
  gitDiff(
    hostId: string,
    worktree: WorktreeId,
    path: string,
    opts: { oldPath?: string | null; staged: boolean; contextLines?: number }
  ): Promise<GitDiffInfo>
  /** Branch, upstream and ahead / behind of a worktree (needs civetd >= 0.1.14). */
  gitBranch(hostId: string, worktree: WorktreeId): Promise<GitBranchInfo>
  /** A git write operation; a discard resolves to the snapshot that undoes it, the rest to null. */
  gitAction(
    hostId: string,
    worktree: WorktreeId,
    action: GitAction
  ): Promise<GitDiscardSnapshot | null>
  /**
   * Every worktree's ahead / behind vs. the default branch and its upstream,
   * and which look stale (civetd >= 0.4.1). `fetch`: `git fetch --prune`
   * first; `review`: also uncommitted changes and pull requests (slower).
   */
  worktreeLifecycle(
    hostId: string,
    repo: RepoId,
    opts: { fetch: boolean; review: boolean }
  ): Promise<RepoLifecycle>
  /** Fetch the default branch, then rebase onto it or merge it in (civetd >= 0.4.1). */
  updateWorktree(
    hostId: string,
    worktree: WorktreeId,
    strategy: UpdateStrategy
  ): Promise<WorktreeUpdate>
  createWorktree(
    hostId: string,
    repo: RepoId,
    branch: string,
    base?: string | null,
    path?: string | null
  ): Promise<WorktreeInfo>
  removeWorktree(hostId: string, worktree: WorktreeId, force: boolean): Promise<void>

  /** One directory level of a worktree (`dir` relative to its root, `''` = root; civetd >= 0.1.10). */
  listFiles(
    hostId: string,
    worktree: WorktreeId,
    dir: string,
    includeIgnored: boolean
  ): Promise<DirListing>
  /** The first `maxBytes` (at most 1 MiB) of a worktree file. */
  readFile(
    hostId: string,
    worktree: WorktreeId,
    path: string,
    maxBytes: number
  ): Promise<FilePreview>
  /** Every tracked and untracked, not ignored file, for quick-open. */
  listAllFiles(hostId: string, worktree: WorktreeId): Promise<FileList>
  /** Create, move, copy, trash or restore worktree files (civetd >= 0.1.15). */
  fileOp(hostId: string, worktree: WorktreeId, op: FileOp): Promise<FileOpResult>
  /**
   * Copy local files and folders (dropped from Finder) into `dir` of a
   * worktree under their own names; `path` of each result is worktree-relative.
   */
  uploadToWorktree(
    hostId: string,
    worktree: WorktreeId,
    dir: string,
    paths: string[]
  ): Promise<DroppedFile[]>
  /** Save a worktree file on this Mac, asking where; the chosen path, or null if cancelled. */
  downloadFile(hostId: string, worktree: WorktreeId, path: string): Promise<string | null>
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
  /**
   * Type `text` into an agent pane and submit it (bracketed paste, then
   * Enter): one pane of a broadcast (civetd >= 0.4.4).
   */
  sendPrompt(hostId: string, pane: PaneId, text: string): Promise<void>
  /** Queue a prompt the daemon sends when the pane's agent is next done (civetd >= 0.4.4). */
  queuePrompt(
    hostId: string,
    pane: PaneId,
    text: string,
    trigger: PromptTrigger
  ): Promise<QueuedPrompt>
  editQueuedPrompt(
    hostId: string,
    id: number,
    text: string,
    trigger: PromptTrigger
  ): Promise<QueuedPrompt>
  cancelQueuedPrompt(hostId: string, id: number): Promise<void>
  /** Every pane's queued prompts (empty from a civetd older than 0.4.4). */
  listQueuedPrompts(hostId: string): Promise<QueuedPrompt[]>
  setPrediction(enabled: boolean): Promise<void>
  /** Write text to the system clipboard (Cmd+C on a terminal selection). */
  writeClipboard(text: string): Promise<void>
  /** The system clipboard's plain text ("" if none), for Paste as File. */
  readClipboard(): Promise<string>

  /** The host's listening ports as last reported (also pushed as `onPorts`). */
  listPorts(hostId: string): Promise<PortsEvent>
  /**
   * Forward `remotePort` on the host to `localhost:localPort` on this Mac
   * (null: the same number if free, else any). Idempotent per remote port.
   */
  forwardPort(hostId: string, remotePort: number, localPort?: number | null): Promise<ForwardView>
  stopForward(hostId: string, remotePort: number): Promise<void>
  listForwards(hostId: string): Promise<ForwardView[]>
  /** Open an `http(s)://localhost` / `127.0.0.1` URL in the default browser. */
  openLocalUrl(url: string): Promise<void>
  /** Open a `https://<host>/<owner>/<repo>/pull/<n>` URL in the browser. */
  openPullRequest(url: string): Promise<void>
  /** Open an `http(s)://` link (clicked in a Markdown preview) in the browser. */
  openWebUrl(url: string): Promise<void>

  onHostState(cb: (e: HostStateEvent) => void): Promise<Unlisten>
  onBootstrapProgress(cb: (e: BootstrapProgressEvent) => void): Promise<Unlisten>
  onDaemonEvent(cb: (e: DaemonEventEnvelope) => void): Promise<Unlisten>
  onAgentAttention(cb: (e: AgentAttentionEvent) => void): Promise<Unlisten>
  onClipboard(cb: (e: ClipboardEvent) => void): Promise<Unlisten>
  /** Edit → Paste as File (⇧⌘V) was chosen, by key or by click. */
  onPasteAsFile(cb: () => void): Promise<Unlisten>
  onPorts(cb: (e: PortsEvent) => void): Promise<Unlisten>
  onForwards(cb: (e: ForwardsEvent) => void): Promise<Unlisten>

  /**
   * Make files dropped on a pane usable from its host: local paths as they
   * are on This Mac, uploaded to the daemon (daemon-side paths) otherwise.
   */
  prepareFileDrop(hostId: string, paths: string[]): Promise<DroppedFile[]>
  /** Store `data` as `name` in a temp dir on the host; resolves to its path there. */
  uploadFile(hostId: string, name: string, data: Uint8Array): Promise<string>
  /** OS file drags over the window (positions in CSS pixels). */
  onFileDrop(cb: (e: FileDropEvent) => void): Promise<Unlisten>
}

export type DroppedFile = { source: string; path: string | null; error: string | null }

export type FileDropEvent =
  | { type: 'over'; x: number; y: number }
  | { type: 'drop'; x: number; y: number; paths: string[] }
  | { type: 'leave' }

type InvokeFn = <T>(
  cmd: string,
  args?: Record<string, unknown> | Uint8Array,
  options?: { headers: Record<string, string> }
) => Promise<T>
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
  /**
   * In-flight attach per pane, so `attach_pane` calls for a pane land in
   * order (the last one's sink wins on the Rust side).
   */
  private attaching = new Map<string, Promise<void>>()
  /**
   * The newest attachment per pane. `detach_pane` isn't per-sink, so only
   * the owner may send it: a stale attachment's cleanup (left before its
   * attach resolved, then re-opened) must not detach its successor.
   */
  private owners = new Map<string, symbol>()

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
  reconnectHost(hostId: string) {
    return this.call<HostSummary>('reconnect_host', { hostId })
  }
  restartDaemon(hostId: string) {
    return this.call<HostSummary>('restart_daemon', { hostId })
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
  listDirs(hostId: string, path: string) {
    return this.call<HostDirListing>('list_dirs', { hostId, path })
  }
  hostStats(hostId: string, since: number | null, processes: boolean) {
    return this.call<HostStats>('host_stats', { hostId, since, processes })
  }
  agentUsage(hostId: string, sinceMs: number) {
    return this.call<AgentUsage>('agent_usage', { hostId, sinceMs })
  }
  listWorktrees(hostId: string, repo: RepoId) {
    return this.call<WorktreeInfo[]>('list_worktrees', { hostId, repo })
  }
  worktreeTemplate(hostId: string, repo: RepoId) {
    return this.call<WorktreeTemplateInfo>('worktree_template', { hostId, repo })
  }
  setWorktreeTemplate(hostId: string, repo: RepoId, template: WorktreeTemplate) {
    return this.call<WorktreeTemplateInfo>('set_worktree_template', { hostId, repo, template })
  }
  setupWorktree(hostId: string, worktree: WorktreeId, size: TermSize) {
    return this.call<WorktreeSetup>('setup_worktree', { hostId, worktree, size })
  }
  listPullRequests(hostId: string, repo: RepoId) {
    return this.call<PullRequestInfo[]>('list_pull_requests', { hostId, repo })
  }
  pullRequestDetail(hostId: string, worktree: WorktreeId, threads: boolean) {
    return this.call<PullRequestDetail | null>('pull_request_detail', {
      hostId,
      worktree,
      threads
    })
  }
  createPullRequest(hostId: string, request: CreatePullRequest) {
    return this.call<PullRequestInfo>('create_pull_request', { hostId, request })
  }
  gitStatus(hostId: string, worktree: WorktreeId) {
    return this.call<GitStatusInfo>('git_status', { hostId, worktree })
  }
  gitDiff(
    hostId: string,
    worktree: WorktreeId,
    path: string,
    opts: { oldPath?: string | null; staged: boolean; contextLines?: number }
  ) {
    return this.call<GitDiffInfo>('git_diff', {
      hostId,
      worktree,
      path,
      oldPath: opts.oldPath ?? null,
      staged: opts.staged,
      contextLines: opts.contextLines ?? 3
    })
  }
  gitBranch(hostId: string, worktree: WorktreeId) {
    return this.call<GitBranchInfo>('git_branch', { hostId, worktree })
  }
  gitAction(hostId: string, worktree: WorktreeId, action: GitAction) {
    return this.call<GitDiscardSnapshot | null>('git_action', { hostId, worktree, action })
  }
  worktreeLifecycle(hostId: string, repo: RepoId, opts: { fetch: boolean; review: boolean }) {
    return this.call<RepoLifecycle>('worktree_lifecycle', { hostId, repo, ...opts })
  }
  updateWorktree(hostId: string, worktree: WorktreeId, strategy: UpdateStrategy) {
    return this.call<WorktreeUpdate>('update_worktree', { hostId, worktree, strategy })
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
  listFiles(hostId: string, worktree: WorktreeId, dir: string, includeIgnored: boolean) {
    return this.call<DirListing>('list_files', { hostId, worktree, dir, includeIgnored })
  }
  readFile(hostId: string, worktree: WorktreeId, path: string, maxBytes: number) {
    return this.call<FilePreview>('read_file', { hostId, worktree, path, maxBytes })
  }
  listAllFiles(hostId: string, worktree: WorktreeId) {
    return this.call<FileList>('list_all_files', { hostId, worktree })
  }
  fileOp(hostId: string, worktree: WorktreeId, op: FileOp) {
    return this.call<FileOpResult>('file_op', { hostId, worktree, op })
  }
  uploadToWorktree(hostId: string, worktree: WorktreeId, dir: string, paths: string[]) {
    return this.call<DroppedFile[]>('upload_to_worktree', { hostId, worktree, dir, paths })
  }
  async downloadFile(hostId: string, worktree: WorktreeId, path: string) {
    const { save } = await import('@tauri-apps/plugin-dialog')
    const dest = await save({
      title: 'Download',
      defaultPath: path.slice(path.lastIndexOf('/') + 1)
    })
    if (!dest) return null
    await this.call<number>('download_file', { hostId, worktree, path, dest })
    return dest
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
    const owner = Symbol('attachment')
    this.owners.set(key, owner)
    const prev = this.attaching.get(key)
    let settle!: () => void
    const turn = new Promise<void>((r) => (settle = r))
    this.attaching.set(key, turn)
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
      if (prev) await prev
      await this.detaching.get(key)?.catch(() => {})
      info = await this.call<SubscribeInfo>('attach_pane', { hostId, pane, onOutput: channel })
    } catch (err) {
      active = false
      if (this.owners.get(key) === owner) this.owners.delete(key)
      throw err
    } finally {
      settle()
      if (this.attaching.get(key) === turn) this.attaching.delete(key)
    }
    return {
      info,
      detach: async () => {
        if (!active) return
        active = false
        channel.onmessage = () => {}
        // A newer attachment replaced this sink; detaching would cut it off.
        if (this.owners.get(key) !== owner) return
        this.owners.delete(key)
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
  sendPrompt(hostId: string, pane: PaneId, text: string) {
    return this.callVoid('send_prompt', { hostId, pane, text })
  }
  queuePrompt(hostId: string, pane: PaneId, text: string, trigger: PromptTrigger) {
    return this.call<QueuedPrompt>('queue_prompt', { hostId, pane, text, trigger })
  }
  editQueuedPrompt(hostId: string, id: number, text: string, trigger: PromptTrigger) {
    return this.call<QueuedPrompt>('edit_queued_prompt', { hostId, id, text, trigger })
  }
  cancelQueuedPrompt(hostId: string, id: number) {
    return this.callVoid('cancel_queued_prompt', { hostId, id })
  }
  listQueuedPrompts(hostId: string) {
    return this.call<QueuedPrompt[]>('list_queued_prompts', { hostId })
  }
  setPrediction(enabled: boolean) {
    return this.callVoid('set_prediction', { enabled })
  }
  writeClipboard(text: string) {
    return this.callVoid('write_clipboard', { text })
  }
  readClipboard() {
    return this.call<string>('read_clipboard')
  }
  listPorts(hostId: string) {
    return this.call<PortsEvent>('list_ports', { hostId })
  }
  forwardPort(hostId: string, remotePort: number, localPort: number | null = null) {
    return this.call<ForwardView>('forward_port', { hostId, remotePort, localPort })
  }
  stopForward(hostId: string, remotePort: number) {
    return this.callVoid('stop_forward', { hostId, remotePort })
  }
  listForwards(hostId: string) {
    return this.call<ForwardView[]>('list_forwards', { hostId })
  }
  openLocalUrl(url: string) {
    return this.callVoid('open_local_url', { url })
  }
  openPullRequest(url: string) {
    return this.callVoid('open_pull_request', { url })
  }
  openWebUrl(url: string) {
    return this.callVoid('open_web_url', { url })
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
  onClipboard(cb: (e: ClipboardEvent) => void) {
    return this.on(CivetEvents.clipboard, cb)
  }
  onPasteAsFile(cb: () => void) {
    return this.on(CivetEvents.pasteAsFile, () => cb())
  }
  onPorts(cb: (e: PortsEvent) => void) {
    return this.on(CivetEvents.ports, cb)
  }
  onForwards(cb: (e: ForwardsEvent) => void) {
    return this.on(CivetEvents.forwards, cb)
  }

  prepareFileDrop(hostId: string, paths: string[]) {
    return this.call<DroppedFile[]>('prepare_file_drop', { hostId, paths })
  }
  async uploadFile(hostId: string, name: string, data: Uint8Array) {
    // Raw body: a JSON array of numbers would be ~4x the bytes.
    try {
      return await this.deps.invoke<string>('upload_file', data, {
        headers: { 'x-civet-host': hostId, 'x-civet-name': name }
      })
    } catch (err) {
      throw CivetError.from(err)
    }
  }
  onFileDrop(cb: (e: FileDropEvent) => void) {
    // Tauri reports physical pixels; elementFromPoint wants CSS pixels.
    return getCurrentWebview().onDragDropEvent(({ payload: p }) => {
      if (p.type === 'leave') return cb({ type: 'leave' })
      const r = window.devicePixelRatio || 1
      const x = p.position.x / r
      const y = p.position.y / r
      cb(p.type === 'drop' ? { type: 'drop', x, y, paths: p.paths } : { type: 'over', x, y })
    })
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
