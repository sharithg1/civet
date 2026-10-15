// In-browser fake of the civet-desktop Rust core, auto-selected when the UI
// is not running inside Tauri (`pnpm dev` in a plain browser). It speaks the
// same CivetBackend interface and emits the same binary records as the real
// channel, so the record parser, frame batching, resize and scrollback paths
// all run for real against it.
//
// Fake host: repos "civet" and "orca" with a few worktrees, an agent pane
// that finishes after a while, and an echo shell per pane. Shell commands:
// help, seq N, status <running|needs|done|idle>, title T, drop, exit.

import { CivetError, type CivetBackend, type PaneOutputHandler, type Unlisten } from './backend'
import { RecordFlag, RecordKind, encodeRecords, type RecordInit } from './terminal/records'
import type {
  AgentAttentionEvent,
  AgentStatus,
  BootstrapProgressEvent,
  BootstrapStep,
  ClipboardEvent,
  DaemonEvent,
  DaemonEventEnvelope,
  DaemonInfo,
  HostStateEvent,
  HostSummary,
  HostTarget,
  LaunchAgentRequest,
  PaneId,
  PaneInfo,
  RepoInfo,
  ScrollbackPageView,
  ScrollbackQuery,
  SpawnPaneRequest,
  TermSize,
  TransportChoice,
  WorktreeInfo
} from './types'

const SYNTHETIC_HISTORY = 3000
const FULL_REPLAY_LINES = 1000
const CHANNEL_FLUSH_MS = 4

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

type Listener<T> = (e: T) => void

class Emitter {
  private map = new Map<string, Set<Listener<unknown>>>()
  on<T>(name: string, cb: Listener<T>): Unlisten {
    let set = this.map.get(name)
    if (!set) this.map.set(name, (set = new Set()))
    set.add(cb as Listener<unknown>)
    return () => set.delete(cb as Listener<unknown>)
  }
  emit<T>(name: string, e: T): void {
    for (const cb of this.map.get(name) ?? []) cb(e)
  }
}

function historyLineVt(n: number): string {
  return n < SYNTHETIC_HISTORY
    ? `\x1b[2m${String(n).padStart(5)}\x1b[0m older output from before you attached (line ${n})\x1b[0m`
    : `\x1b[2m${String(n).padStart(5)}\x1b[0m line ${n}\x1b[0m`
}

/** A tiny line-editing echo shell that produces screen records. */
class MockShell {
  private sinks = new Set<PaneOutputHandler>()
  private pending: RecordInit[] = []
  private flushTimer: ReturnType<typeof setTimeout> | null = null
  private transcript = ''
  private line = ''
  private newlines = 0
  state = 1
  inputSeq = 0

  constructor(
    readonly pane: MockPane,
    private readonly prompt: string,
    banner: string
  ) {
    this.transcript = banner + this.prompt
    this.newlines = countNewlines(banner)
  }

  get historyLines(): number {
    return SYNTHETIC_HISTORY + Math.max(0, this.newlines - this.pane.info.size.rows + 1)
  }

  attach(sink: PaneOutputHandler): void {
    this.sinks.add(sink)
    const historyStart = Math.max(0, this.historyLines - FULL_REPLAY_LINES)
    let replay = ''
    for (let n = historyStart; n < SYNTHETIC_HISTORY; n++) replay += historyLineVt(n) + '\r\n'
    // Deliver the Full only to the new subscriber.
    const { cols, rows } = this.pane.info.size
    const full = encodeRecords([
      {
        kind: RecordKind.Screen,
        flags: RecordFlag.Full,
        cols,
        rows,
        state: this.state,
        ackedInputSeq: this.inputSeq,
        historyStart,
        historyLines: this.historyLines,
        payload: '\x1bc' + replay + this.transcript
      }
    ])
    setTimeout(() => sink(full), 0)
  }

  detach(sink: PaneOutputHandler): void {
    this.sinks.delete(sink)
  }

  output(text: string, extra: Partial<RecordInit> = {}): void {
    this.transcript += text
    if (this.transcript.length > 200_000) this.transcript = this.transcript.slice(-100_000)
    const before = this.historyLines
    this.newlines += countNewlines(text)
    this.state += 1
    const { cols, rows } = this.pane.info.size
    this.queue({
      kind: RecordKind.Screen,
      flags: 0,
      cols,
      rows,
      state: this.state,
      ackedInputSeq: this.inputSeq,
      historyStart: before,
      historyLines: this.historyLines,
      payload: text,
      ...extra
    })
  }

  closed(): void {
    this.queue({ kind: RecordKind.PaneClosed, state: this.state })
  }

  resize(size: TermSize): void {
    this.output('', { flags: RecordFlag.Resized, cols: size.cols, rows: size.rows })
  }

  input(data: string): void {
    this.inputSeq += 1
    let echo = ''
    for (const ch of data) {
      if (ch === '\r') {
        echo += '\r\n'
        this.flushEcho(echo)
        echo = ''
        const cmd = this.line.trim()
        this.line = ''
        this.pane.run(cmd)
        continue
      }
      if (ch === '\x7f' || ch === '\b') {
        if (this.line.length > 0) {
          this.line = this.line.slice(0, -1)
          echo += '\b \b'
        }
        continue
      }
      if (ch === '\x03') {
        this.line = ''
        echo += '^C\r\n' + this.prompt
        continue
      }
      if (ch >= ' ' && ch !== '\x7f') {
        this.line += ch
        echo += ch
      }
    }
    this.flushEcho(echo)
  }

  showPrompt(): void {
    this.output(this.prompt)
  }

  private flushEcho(echo: string): void {
    if (echo) this.output(echo)
  }

  /** Same batching rule as the Rust side: flush 4 ms after the first queued record. */
  private queue(r: RecordInit): void {
    this.pending.push(r)
    this.flushTimer ??= setTimeout(() => {
      this.flushTimer = null
      const msg = encodeRecords(this.pending)
      this.pending = []
      for (const sink of this.sinks) sink(msg)
    }, CHANNEL_FLUSH_MS)
  }
}

function countNewlines(s: string): number {
  let n = 0
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) === 10) n++
  return n
}

class MockPane {
  shell: MockShell
  constructor(
    readonly host: MockHost,
    public info: PaneInfo
  ) {
    const cwd = info.cwd.replace(/^\/home\/dev/, '~')
    const prompt = `\x1b[32mdev@devbox\x1b[0m:\x1b[34m${cwd}\x1b[0m$ `
    const banner = info.agent
      ? `\x1b[1;35m✻ ${info.agent} (mock agent)\x1b[0m\r\n` +
        `  Working on: ${info.title}\r\n` +
        `  This is the Civet mock backend; the agent "finishes" in ~12 s.\r\n\r\n`
      : `\x1b[1mCivet mock shell\x1b[0m — type \x1b[33mhelp\x1b[0m for commands.\r\n`
    this.shell = new MockShell(this, prompt, banner)
  }

  run(cmd: string): void {
    const [name, ...args] = cmd.split(/\s+/)
    const out = (s: string) => this.shell.output(s)
    switch (name) {
      case '':
        break
      case 'help':
        out(
          'Commands:\r\n' +
            '  seq N                print N numbered lines (scrollback test)\r\n' +
            '  status S             set agent status: running | needs | done | idle\r\n' +
            '  title T              set the pane title\r\n' +
            '  drop                 simulate a network drop (reconnecting banner)\r\n' +
            '  exit                 exit the pane\r\n'
        )
        break
      case 'seq': {
        const n = Math.min(Number(args[0] ?? 100) || 100, 50_000)
        let s = ''
        for (let i = 1; i <= n; i++) s += `${i}\r\n`
        out(s)
        break
      }
      case 'status': {
        const map: Record<string, AgentStatus> = {
          running: 'running',
          needs: 'needsInput',
          needsinput: 'needsInput',
          done: 'done',
          idle: 'idle'
        }
        const status = map[(args[0] ?? '').toLowerCase()]
        if (status) this.host.setStatus(this.info.id, status, `set from the mock shell`)
        else out('usage: status running|needs|done|idle\r\n')
        break
      }
      case 'title':
        this.info = { ...this.info, title: args.join(' ') || 'shell' }
        this.host.event({ paneTitle: { pane: this.info.id, title: this.info.title } })
        break
      case 'drop':
        out('simulating a 2.5 s network outage…\r\n')
        this.host.backend.simulateNetworkDrop(this.host.summary.hostId)
        break
      case 'exit':
        this.host.exitPane(this.info.id, 0)
        return
      default:
        out(`mock: ${name}: echoing "${args.join(' ')}"\r\n`)
    }
    this.shell.showPrompt()
  }
}

class MockHost {
  repos: RepoInfo[]
  worktrees = new Map<string, WorktreeInfo[]>()
  panes = new Map<PaneId, MockPane>()
  private nextPaneId = 1

  constructor(
    readonly backend: MockBackend,
    public summary: HostSummary
  ) {
    this.repos = [
      { id: 'civet', name: 'civet', path: '/home/dev/src/civet', defaultBranch: 'main' },
      { id: 'orca', name: 'orca', path: '/home/dev/src/orca', defaultBranch: 'main' }
    ]
    const wt = (repo: string, branch: string, isMain = false): WorktreeInfo => ({
      id: isMain ? `/home/dev/src/${repo}` : `/home/dev/src/${repo}-worktrees/${branch}`,
      repo,
      path: isMain ? `/home/dev/src/${repo}` : `/home/dev/src/${repo}-worktrees/${branch}`,
      branch,
      head: 'a1b2c3d',
      isMain,
      locked: false
    })
    this.worktrees.set('civet', [
      wt('civet', 'main', true),
      wt('civet', 'quic-resume'),
      wt('civet', 'scrollback-paging')
    ])
    this.worktrees.set('orca', [wt('orca', 'main', true), wt('orca', 'fix-wake-reconnect')])

    const qr = this.worktrees.get('civet')![1]!
    this.addPane({ worktree: qr.id, cwd: qr.path, title: 'Implement QUIC resume', agent: 'claude' })
    const sp = this.worktrees.get('civet')![2]!
    this.addPane({ worktree: sp.id, cwd: sp.path, title: 'shell', agent: null })
  }

  addPane(p: {
    worktree: string | null
    cwd: string
    title: string
    agent: string | null
    command?: string[]
    size?: TermSize
  }): MockPane {
    const id = this.nextPaneId++
    const info: PaneInfo = {
      id,
      worktree: p.worktree,
      cwd: p.cwd,
      command: p.command ?? (p.agent ? [p.agent] : ['bash', '-l']),
      title: p.title,
      size: p.size ?? { cols: 100, rows: 30, pixelWidth: 0, pixelHeight: 0 },
      agent: p.agent,
      status: p.agent ? 'running' : 'unknown',
      exitCode: null,
      exited: false,
      createdAtMs: Date.now(),
      state: 1
    }
    const pane = new MockPane(this, info)
    this.panes.set(id, pane)
    if (p.agent) {
      // Fake agent run: some output, then done.
      const tick = setInterval(() => {
        if (!this.panes.has(id) || pane.info.status !== 'running') return clearInterval(tick)
        pane.shell.output(`\x1b[2m  … ${randomStep()}\x1b[0m\r\n`)
      }, 1500)
      setTimeout(() => {
        clearInterval(tick)
        if (!this.panes.has(id) || pane.info.status !== 'running') return
        pane.shell.output('\r\n\x1b[1;32m✓ Done.\x1b[0m All tasks complete.\r\n')
        pane.shell.showPrompt()
        this.setStatus(id, 'done', 'Agent finished: all tasks complete')
      }, 12_000)
    }
    return pane
  }

  event(event: DaemonEvent): void {
    this.backend.emitter.emit<DaemonEventEnvelope>('civet://daemon-event', {
      hostId: this.summary.hostId,
      event
    })
  }

  setStatus(id: PaneId, status: AgentStatus, message: string | null): void {
    const pane = this.panes.get(id)
    if (!pane) return
    const previous = pane.info.status
    if (previous === status) return
    pane.info = { ...pane.info, status }
    this.event({
      agentStatus: { pane: id, status, previous, source: 'client', message, atMs: Date.now() }
    })
    if (status === 'done' || status === 'needsInput') {
      const wt = this.allWorktrees().find((w) => w.id === pane.info.worktree)
      this.backend.emitter.emit<AgentAttentionEvent>('civet://agent-attention', {
        hostId: this.summary.hostId,
        pane: id,
        status,
        previous,
        title: pane.info.title || wt?.branch || `pane ${id}`,
        worktree: pane.info.worktree,
        message
      })
    }
  }

  exitPane(id: PaneId, code: number): void {
    const pane = this.panes.get(id)
    if (!pane) return
    pane.shell.output('\r\n[exited]\r\n')
    pane.info = { ...pane.info, exited: true, exitCode: code }
    this.event({ paneExited: { pane: id, exitCode: code } })
    this.setStatus(id, 'done', `exit ${code}`)
  }

  killPane(id: PaneId): void {
    const pane = this.panes.get(id)
    if (!pane) throw new CivetError('notFound', `pane ${id} not found`)
    pane.shell.closed()
    this.panes.delete(id)
    this.event({ paneClosed: { pane: id } })
  }

  allWorktrees(): WorktreeInfo[] {
    return [...this.worktrees.values()].flat()
  }

  pane(id: PaneId): MockPane {
    const p = this.panes.get(id)
    if (!p) throw new CivetError('notFound', `pane ${id} not found`)
    return p
  }
}

const STEPS = [
  'reading crates/civet-transport/src/session.rs',
  'running cargo test -p civet-transport',
  'editing resume token handling',
  'checking reconnect timings',
  'writing property tests'
]
function randomStep(): string {
  return STEPS[Math.floor(Math.random() * STEPS.length)]!
}

export class MockBackend implements CivetBackend {
  readonly kind = 'mock' as const
  readonly emitter = new Emitter()
  private hosts = new Map<string, MockHost>()

  private host(hostId: string): MockHost {
    const h = this.hosts.get(hostId)
    if (!h) throw new CivetError('noSuchHost', `no such host: ${hostId}`)
    return h
  }

  private hostState(hostId: string, e: Omit<HostStateEvent, 'hostId'>): void {
    const h = this.hosts.get(hostId)
    if (h) h.summary = { ...h.summary, state: e.state, transport: e.transport }
    this.emitter.emit<HostStateEvent>('civet://host-state', { hostId, ...e })
  }

  private progress(hostId: string, step: BootstrapStep, message: string | null = null): void {
    this.emitter.emit<BootstrapProgressEvent>('civet://bootstrap-progress', {
      hostId,
      step,
      message
    })
  }

  async connectHost(target: HostTarget, transport: TransportChoice = 'auto') {
    const hostId = target.kind === 'local' ? 'local' : target.destination.trim()
    if (!hostId) throw new CivetError('bootstrap', 'empty ssh destination')
    const existing = this.hosts.get(hostId)
    if (existing && existing.summary.state === 'connected') return existing.summary
    const label = target.kind === 'local' ? 'This Mac' : hostId
    const summary: HostSummary = {
      hostId,
      label,
      state: 'connecting',
      transport: null,
      daemonVersion: null,
      instanceId: null
    }
    const host = existing ?? new MockHost(this, summary)
    host.summary = summary
    this.hosts.set(hostId, host)
    this.hostState(hostId, { state: 'connecting', transport: null, rttMs: null, message: null })
    const steps: [BootstrapStep, string][] = [
      ['probe', `ssh ${hostId} civetd version`],
      ['start', 'starting civetd (reattached: false)'],
      ['resolve', 'udp port 60017'],
      ['dial', 'QUIC handshake']
    ]
    for (const [step, msg] of steps) {
      await sleep(180)
      this.progress(hostId, step, msg)
    }
    if (hostId.includes('fail')) {
      await sleep(200)
      this.hostState(hostId, {
        state: 'failed',
        transport: null,
        rttMs: null,
        message: 'ssh: connect to host fail: Connection refused'
      })
      throw new CivetError('bootstrap', `ssh ${hostId}: Connection refused (mock)`)
    }
    const stream = transport === 'ssh' || hostId.includes('udp-blocked')
    if (stream) {
      await sleep(transport === 'ssh' ? 100 : 600)
      this.progress(hostId, 'fallback', 'using ssh stdio tunnel')
    }
    await sleep(150)
    host.summary = {
      ...summary,
      state: 'connected',
      transport: stream ? 'stream' : 'quic',
      daemonVersion: '0.1.0-mock',
      instanceId: '17179869184'
    }
    this.hostState(hostId, {
      state: 'connected',
      transport: host.summary.transport,
      rttMs: 42,
      message: null
    })
    return host.summary
  }

  async disconnectHost(hostId: string) {
    this.host(hostId)
    this.hostState(hostId, { state: 'disconnected', transport: null, rttMs: null, message: null })
    this.hosts.delete(hostId)
  }

  async listHosts() {
    return [...this.hosts.values()].map((h) => h.summary)
  }

  async notifyNetworkChange() {}

  /** Dev helper: mimic a stall the Rust core would report after its 1 s threshold. */
  simulateNetworkDrop(hostId: string, outageMs = 2500): void {
    const h = this.hosts.get(hostId)
    if (!h) return
    const transport = h.summary.transport
    setTimeout(
      () => this.hostState(hostId, { state: 'reconnecting', transport, rttMs: null, message: null }),
      1000
    )
    setTimeout(
      () => this.hostState(hostId, { state: 'connected', transport, rttMs: 38, message: null }),
      outageMs
    )
  }

  async daemonInfo(hostId: string): Promise<DaemonInfo> {
    this.host(hostId)
    return {
      daemonVersion: '0.1.0-mock',
      protoVersion: 1,
      pid: 4242,
      instanceId: 17179869184,
      startedAtMs: Date.now() - 3_600_000,
      hostname: 'devbox'
    }
  }

  async ping(hostId: string) {
    this.host(hostId)
    return { rttMs: 40 + Math.round(Math.random() * 5) }
  }

  async listRepos(hostId: string) {
    return [...this.host(hostId).repos]
  }

  async addRepo(hostId: string, path: string) {
    const h = this.host(hostId)
    const name = path.replace(/\/+$/, '').split('/').pop() || path
    if (h.repos.some((r) => r.path === path)) {
      throw new CivetError('alreadyExists', `repo ${path} already added`)
    }
    const repo: RepoInfo = { id: name, name, path, defaultBranch: 'main' }
    h.repos.push(repo)
    h.worktrees.set(repo.id, [
      { id: path, repo: repo.id, path, branch: 'main', head: 'f00dbab', isMain: true, locked: false }
    ])
    h.event('reposChanged')
    return repo
  }

  async removeRepo(hostId: string, repo: string) {
    const h = this.host(hostId)
    h.repos = h.repos.filter((r) => r.id !== repo)
    h.worktrees.delete(repo)
    h.event('reposChanged')
  }

  async listWorktrees(hostId: string, repo: string) {
    return [...(this.host(hostId).worktrees.get(repo) ?? [])]
  }

  async createWorktree(
    hostId: string,
    repo: string,
    branch: string,
    _base?: string | null,
    path?: string | null
  ) {
    const h = this.host(hostId)
    const r = h.repos.find((x) => x.id === repo)
    if (!r) throw new CivetError('notFound', `repo ${repo} not found`)
    const list = h.worktrees.get(repo) ?? []
    if (list.some((w) => w.branch === branch)) {
      throw new CivetError('alreadyExists', `branch ${branch} already has a worktree`)
    }
    const wtPath = path ?? `${r.path}-worktrees/${branch}`
    const wt: WorktreeInfo = {
      id: wtPath,
      repo,
      path: wtPath,
      branch,
      head: '0badc0d',
      isMain: false,
      locked: false
    }
    h.worktrees.set(repo, [...list, wt])
    h.event({ worktreesChanged: { repo } })
    return wt
  }

  async removeWorktree(hostId: string, worktree: string, _force: boolean) {
    const h = this.host(hostId)
    for (const [repo, list] of h.worktrees) {
      if (list.some((w) => w.id === worktree)) {
        h.worktrees.set(
          repo,
          list.filter((w) => w.id !== worktree)
        )
        h.event({ worktreesChanged: { repo } })
      }
    }
  }

  async listPanes(hostId: string) {
    return [...this.host(hostId).panes.values()].map((p) => p.info)
  }

  async spawnPane(hostId: string, request: SpawnPaneRequest) {
    const h = this.host(hostId)
    const wt = h.allWorktrees().find((w) => w.id === request.worktree)
    const pane = h.addPane({
      worktree: request.worktree,
      cwd: request.cwd ?? wt?.path ?? '/home/dev',
      title: request.title ?? 'shell',
      agent: request.agent,
      command: request.command ? [request.command.program, ...request.command.args] : undefined,
      size: request.size
    })
    h.event({ paneCreated: pane.info })
    return pane.info
  }

  async launchAgent(hostId: string, request: LaunchAgentRequest) {
    const h = this.host(hostId)
    await sleep(300)
    const worktree = await this.createWorktree(
      hostId,
      request.repo,
      request.branch,
      request.base,
      request.worktreePath
    )
    const pane = h.addPane({
      worktree: worktree.id,
      cwd: worktree.path,
      title: request.title ?? request.branch,
      agent: request.agent ?? request.command.program,
      command: [request.command.program, ...request.command.args],
      size: request.size
    })
    h.event({ paneCreated: pane.info })
    return { worktree, pane: pane.info }
  }

  async attachPane(hostId: string, pane: PaneId, onOutput: PaneOutputHandler) {
    const p = this.host(hostId).pane(pane)
    let active = true
    const sink: PaneOutputHandler = (m) => active && onOutput(m)
    p.shell.attach(sink)
    return {
      info: {
        pane,
        state: p.shell.state,
        size: p.info.size,
        historyLines: p.shell.historyLines,
        lastInputSeq: p.shell.inputSeq
      },
      detach: async () => {
        active = false
        p.shell.detach(sink)
      }
    }
  }

  async writeInput(hostId: string, pane: PaneId, data: string) {
    const p = this.host(hostId).pane(pane)
    if (p.info.exited) return
    p.shell.input(data)
  }

  async writeInputBinary(hostId: string, pane: PaneId, data: string) {
    return this.writeInput(hostId, pane, data)
  }

  async resizePane(hostId: string, pane: PaneId, size: TermSize) {
    const h = this.host(hostId)
    const p = h.pane(pane)
    if (p.info.size.cols === size.cols && p.info.size.rows === size.rows) return
    p.info = { ...p.info, size }
    p.shell.resize(size)
    h.event({ paneResized: { pane, size } })
  }

  async killPane(hostId: string, pane: PaneId) {
    this.host(hostId).killPane(pane)
  }

  async fetchScrollback(
    hostId: string,
    pane: PaneId,
    query: ScrollbackQuery
  ): Promise<ScrollbackPageView> {
    const p = this.host(hostId).pane(pane)
    await sleep(120)
    const endAvailable = p.shell.historyLines
    let start: number
    let end: number
    if ('before' in query) {
      end = Math.min(query.before.end ?? endAvailable, endAvailable)
      start = Math.max(0, end - query.before.limit)
    } else {
      start = Math.max(0, query.range.start)
      end = Math.min(query.range.end, endAvailable)
    }
    const lines: string[] = []
    for (let n = start; n < end; n++) lines.push(historyLineVt(n))
    return {
      start,
      oldestAvailable: 0,
      endAvailable,
      lines,
      wrapped: lines.map(() => false)
    }
  }

  async setAgentStatus(hostId: string, pane: PaneId, status: AgentStatus) {
    this.host(hostId).setStatus(pane, status, null)
  }

  async setPrediction(_enabled: boolean) {}

  async writeClipboard(text: string) {
    await navigator.clipboard?.writeText(text)
  }

  async onHostState(cb: (e: HostStateEvent) => void) {
    return this.emitter.on('civet://host-state', cb)
  }
  async onBootstrapProgress(cb: (e: BootstrapProgressEvent) => void) {
    return this.emitter.on('civet://bootstrap-progress', cb)
  }
  async onDaemonEvent(cb: (e: DaemonEventEnvelope) => void) {
    return this.emitter.on('civet://daemon-event', cb)
  }
  async onAgentAttention(cb: (e: AgentAttentionEvent) => void) {
    return this.emitter.on('civet://agent-attention', cb)
  }
  async onClipboard(cb: (e: ClipboardEvent) => void) {
    return this.emitter.on('civet://clipboard', cb)
  }
}
