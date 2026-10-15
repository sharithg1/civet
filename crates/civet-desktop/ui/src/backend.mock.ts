// In-browser fake of the civet-desktop Rust core, auto-selected when the UI
// is not running inside Tauri (`pnpm dev` in a plain browser). It speaks the
// same CivetBackend interface and emits the same binary records as the real
// channel, so the record parser, frame batching, resize and scrollback paths
// all run for real against it.
//
// Fake host: repos "civet" and "orca" with a few worktrees, an agent pane
// that finishes after a while, agents waiting for input / stopped with an
// error (to show every v3 status dot), and an echo shell per pane. Shell commands:
// help, seq N, status <running|needs|done|idle>, title T, drop, fail, replaced, exit.
// A host whose name contains "old" runs an older civetd with live panes (the
// "upgrade available" flow; Restart daemon starts a fresh one), "fail" never
// connects, "udp-blocked" falls back to the SSH tunnel.

import {
  CivetError,
  type CivetBackend,
  type DroppedFile,
  type FileDropEvent,
  type PaneOutputHandler,
  type Unlisten
} from './backend'
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
  ForwardView,
  ForwardsEvent,
  HostStateEvent,
  HostSummary,
  HostTarget,
  LaunchAgentRequest,
  PaneId,
  PaneInfo,
  PortsEvent,
  RepoInfo,
  ScrollbackPageView,
  ScrollbackQuery,
  SpawnPaneRequest,
  TermSize,
  TransportChoice,
  WorktreeInfo,
  PullRequestInfo
} from './types'

/** This app's civetd version and the older one an "old" mock host runs. */
export const MOCK_VERSION = '0.1.2'
export const MOCK_OLD_VERSION = '0.1.1'
export const MOCK_REPLACED_MESSAGE =
  "another Civet window or client took over this host's connection; reconnect here to take it back"

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
            '  fail                 simulate a lost connection (failed banner)\r\n' +
            '  replaced             simulate another client taking the host over\r\n' +
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
      case 'fail':
        out('simulating a failed connection…\r\n')
        this.host.backend.simulateFailure(this.host.summary.hostId, 'connection lost: no route to host (mock)')
        break
      case 'replaced':
        out('simulating another client taking over…\r\n')
        this.host.backend.simulateFailure(this.host.summary.hostId, MOCK_REPLACED_MESSAGE)
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
    public summary: HostSummary,
    seed = true
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
      wt('civet', 'scrollback-paging'),
      wt('civet', 'NO-TICKET/workspace-v3'),
      wt('civet', 'fix-flaky-e2e')
    ])
    this.worktrees.set('orca', [wt('orca', 'main', true), wt('orca', 'fix-wake-reconnect')])
    // A freshly (re)started daemon has no panes.
    if (!seed) return

    const qr = this.worktrees.get('civet')![1]!
    this.addPane({ worktree: qr.id, cwd: qr.path, title: 'Implement QUIC resume', agent: 'claude' })
    const sp = this.worktrees.get('civet')![2]!
    this.addPane({ worktree: sp.id, cwd: sp.path, title: 'shell', agent: null })
    // An agent waiting for permission, a second idle one and a shell.
    const v3 = this.worktrees.get('civet')![3]!
    this.addPane({
      worktree: v3.id,
      cwd: v3.path,
      title: '✳ Redesign the workspace chrome',
      agent: 'claude',
      status: 'needsInput'
    })
    this.addPane({ worktree: v3.id, cwd: v3.path, title: 'Draft the PR description', agent: 'claude', status: 'idle' })
    this.addPane({ worktree: v3.id, cwd: v3.path, title: 'Review the tab layout', agent: 'codex', command: ['codex'], status: 'idle' })
    this.addPane({ worktree: v3.id, cwd: v3.path, title: 'zsh', agent: null })
    // An agent that stopped with an error.
    const fl = this.worktrees.get('civet')![4]!
    const failed = this.addPane({ worktree: fl.id, cwd: fl.path, title: 'Fix the flaky reconnect e2e', agent: 'claude', status: 'idle' })
    failed.shell.output('\x1b[31m● Stopped after 3 consecutive test failures.\x1b[0m\r\n')
    failed.info = { ...failed.info, exited: true, exitCode: 1 }
  }

  addPane(p: {
    worktree: string | null
    cwd: string
    title: string
    agent: string | null
    command?: string[]
    size?: TermSize
    /** Initial agent status; only a `running` agent fakes work and finishes. */
    status?: AgentStatus
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
      status: p.agent ? (p.status ?? 'running') : 'unknown',
      exitCode: null,
      exited: false,
      createdAtMs: Date.now(),
      state: 1
    }
    const pane = new MockPane(this, info)
    this.panes.set(id, pane)
    if (p.agent && info.status === 'running') {
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
    // Like the Rust core: only agent panes ask for attention.
    if ((status === 'done' || status === 'needsInput') && pane.info.agent) {
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

/** Local ports the mock pretends are already taken on this Mac. */
const MOCK_TAKEN_LOCALLY = new Set([3001, 8080])

export class MockBackend implements CivetBackend {
  readonly kind = 'mock' as const
  readonly emitter = new Emitter()
  private hosts = new Map<string, MockHost>()
  /** hostId → remote port → forward */
  private forwards = new Map<string, Map<number, ForwardView>>()
  private lastPorts = new Map<string, string>()

  constructor() {
    this.emitter.on<DaemonEventEnvelope>('civet://daemon-event', ({ hostId }) => this.refreshPorts(hostId))
  }
  /** What each host was connected with (reconnect_host / restart_daemon reuse it). */
  readonly targets = new Map<string, { target: HostTarget; transport: TransportChoice }>()

  private host(hostId: string): MockHost {
    const h = this.hosts.get(hostId)
    if (!h) throw new CivetError('noSuchHost', `no such host: ${hostId}`)
    return h
  }

  private hostState(hostId: string, e: Omit<HostStateEvent, 'hostId' | 'upgrade'>): void {
    const h = this.hosts.get(hostId)
    if (h) h.summary = { ...h.summary, state: e.state, transport: e.transport }
    this.emitter.emit<HostStateEvent>('civet://host-state', {
      hostId,
      upgrade: h?.summary.upgrade ?? null,
      ...e
    })
  }

  private progress(hostId: string, step: BootstrapStep, message: string | null = null): void {
    this.emitter.emit<BootstrapProgressEvent>('civet://bootstrap-progress', {
      hostId,
      step,
      message
    })
  }

  async connectHost(target: HostTarget, transport: TransportChoice = 'auto') {
    return this.connect(target, transport, false)
  }

  async reconnectHost(hostId: string) {
    const t = this.targets.get(hostId) ?? (hostId === 'local' ? { target: { kind: 'local' } as const, transport: 'auto' as const } : null)
    if (!t) throw new CivetError('noSuchHost', `no host "${hostId}" (connect first)`)
    return this.connect(t.target, t.transport, false)
  }

  async restartDaemon(hostId: string) {
    const t = this.targets.get(hostId) ?? (hostId === 'local' ? { target: { kind: 'local' } as const, transport: 'auto' as const } : null)
    if (!t) throw new CivetError('noSuchHost', `no host "${hostId}" (connect first)`)
    const old = this.hosts.get(hostId)
    if (old) {
      for (const id of [...old.panes.keys()]) old.killPane(id)
      this.hosts.delete(hostId)
    }
    return this.connect(t.target, t.transport, true)
  }

  private async connect(target: HostTarget, transport: TransportChoice, replace: boolean) {
    const hostId = target.kind === 'local' ? 'local' : target.destination.trim()
    if (!hostId) throw new CivetError('bootstrap', 'empty ssh destination')
    this.targets.set(hostId, { target, transport })
    const existing = this.hosts.get(hostId)
    if (existing && existing.summary.state === 'connected') return existing.summary
    const label = target.kind === 'local' ? 'This Mac' : hostId
    const summary: HostSummary = {
      hostId,
      label,
      state: 'connecting',
      transport: null,
      daemonVersion: null,
      instanceId: null,
      upgrade: null
    }
    const host = existing ?? new MockHost(this, summary, !replace)
    host.summary = summary
    this.hosts.set(hostId, host)
    this.hostState(hostId, { state: 'connecting', transport: null, rttMs: null, message: null })
    const steps: [BootstrapStep, string][] = [
      ['probe', `ssh ${hostId} civetd version`],
      ['start', replace ? 'restarting civetd (--replace)' : 'starting civetd (reattached: false)'],
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
    // An "old" host kept its older daemon because it has live panes.
    const old = hostId.includes('old') && !replace && host.panes.size > 0
    host.summary = {
      ...summary,
      state: 'connected',
      transport: stream ? 'stream' : 'quic',
      daemonVersion: old ? MOCK_OLD_VERSION : MOCK_VERSION,
      instanceId: replace ? '17179869185' : '17179869184',
      upgrade: old
        ? { runningVersion: MOCK_OLD_VERSION, availableVersion: MOCK_VERSION, livePanes: host.panes.size }
        : null
    }
    this.hostState(hostId, {
      state: 'connected',
      transport: host.summary.transport,
      rttMs: 42,
      message: null
    })
    this.refreshPorts(hostId)
    return host.summary
  }

  /** Sample listening ports derived from the host's live panes: a dev server per agent, one per shell, plus a database. */
  private mockPorts(hostId: string): PortsEvent {
    const h = this.hosts.get(hostId)
    if (!h || h.summary.state !== 'connected') {
      return { hostId, status: 'pending', message: null, ports: [] }
    }
    if (h.summary.transport === 'stream') {
      return {
        hostId,
        status: 'unsupported',
        message: 'port forwarding needs the QUIC transport (this host is connected over the SSH tunnel)',
        ports: []
      }
    }
    const live = [...h.panes.values()].map((p) => p.info).filter((p) => !p.exited)
    const ports: PortsEvent['ports'] = []
    const agents = live.filter((p) => p.agent)
    const shells = live.filter((p) => !p.agent)
    agents.slice(0, 3).forEach((p, i) => {
      ports.push({
        port: 5173 + i,
        addrs: ['::1'],
        pid: 4100 + p.id,
        process: 'node',
        command: 'node node_modules/.bin/vite --port ' + (5173 + i),
        pane: p.id,
        worktree: p.worktree
      })
    })
    if (agents[0]) {
      ports.push({
        port: 6006,
        addrs: ['127.0.0.1'],
        pid: 4200 + agents[0].id,
        process: 'node',
        command: 'node node_modules/.bin/storybook dev -p 6006',
        pane: agents[0].id,
        worktree: agents[0].worktree
      })
    }
    if (shells[0]) {
      ports.push({
        port: 3001,
        addrs: ['0.0.0.0'],
        pid: 4300 + shells[0].id,
        process: 'node',
        command: 'node server.js',
        pane: shells[0].id,
        worktree: shells[0].worktree
      })
    }
    ports.push({
      port: 5432,
      addrs: ['127.0.0.1', '::1'],
      pid: 812,
      process: 'postgres',
      command: '/usr/lib/postgresql/16/bin/postgres -D /var/lib/postgresql/16/main',
      pane: null,
      worktree: null
    })
    ports.sort((a, b) => a.port - b.port)
    return { hostId, status: 'watching', message: null, ports }
  }

  /** Emit `civet://ports` if the sample ports changed (after connect and every pane event). */
  private refreshPorts(hostId: string): void {
    if (!this.hosts.has(hostId)) return
    const e = this.mockPorts(hostId)
    const json = JSON.stringify(e)
    if (this.lastPorts.get(hostId) === json) return
    this.lastPorts.set(hostId, json)
    this.emitter.emit<PortsEvent>('civet://ports', e)
  }

  private forwardsOf(hostId: string): Map<number, ForwardView> {
    let m = this.forwards.get(hostId)
    if (!m) this.forwards.set(hostId, (m = new Map()))
    return m
  }

  private emitForwards(hostId: string): void {
    const forwards = [...this.forwardsOf(hostId).values()].sort((a, b) => a.remotePort - b.remotePort)
    this.emitter.emit<ForwardsEvent>('civet://forwards', { hostId, forwards })
  }

  async listPorts(hostId: string) {
    this.host(hostId)
    return this.mockPorts(hostId)
  }

  async forwardPort(hostId: string, remotePort: number, localPort: number | null = null) {
    this.host(hostId)
    const m = this.forwardsOf(hostId)
    const existing = m.get(remotePort)
    if (existing) {
      if (localPort != null && localPort !== existing.localPort) {
        throw new CivetError('alreadyExists', `port ${remotePort} is already forwarded to localhost:${existing.localPort}`)
      }
      return existing
    }
    const used = new Set([...m.values()].map((f) => f.localPort))
    if (localPort != null && (MOCK_TAKEN_LOCALLY.has(localPort) || used.has(localPort))) {
      throw new CivetError('alreadyExists', `localhost:${localPort} is already in use on this Mac`)
    }
    let local = localPort ?? remotePort
    if (localPort == null && (MOCK_TAKEN_LOCALLY.has(local) || used.has(local))) {
      local = 51842
      while (used.has(local)) local++
    }
    const f: ForwardView = { remotePort, localPort: local, connections: 0, lastError: null }
    m.set(remotePort, f)
    this.emitForwards(hostId)
    return f
  }

  async stopForward(hostId: string, remotePort: number) {
    if (!this.forwardsOf(hostId).delete(remotePort)) {
      throw new CivetError('notFound', `port ${remotePort} is not forwarded`)
    }
    this.emitForwards(hostId)
  }

  async listForwards(hostId: string) {
    return [...this.forwardsOf(hostId).values()]
  }

  async openLocalUrl(url: string) {
    window.open(url, '_blank', 'noopener')
  }

  async openPullRequest(url: string) {
    window.open(url, '_blank', 'noopener')
  }

  async disconnectHost(hostId: string) {
    this.host(hostId)
    this.hostState(hostId, { state: 'disconnected', transport: null, rttMs: null, message: null })
    this.hosts.delete(hostId)
    this.lastPorts.delete(hostId)
    if (this.forwards.delete(hostId)) this.emitter.emit<ForwardsEvent>('civet://forwards', { hostId, forwards: [] })
  }

  async listHosts() {
    return [...this.hosts.values()].map((h) => h.summary)
  }

  async notifyNetworkChange() {}

  /** Dev helper: the connection failed for good (needs a user Reconnect). */
  simulateFailure(hostId: string, message: string): void {
    if (!this.hosts.has(hostId)) return
    setTimeout(() => this.hostState(hostId, { state: 'failed', transport: null, rttMs: null, message }), 300)
  }

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
      daemonVersion: this.host(hostId).summary.daemonVersion ?? MOCK_VERSION,
      protoVersion: 1,
      pid: 4242,
      instanceId: this.host(hostId).summary.instanceId ?? '17179869184',
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

  async listPullRequests(_hostId: string, repo: string): Promise<PullRequestInfo[]> {
    if (repo !== 'civet') return []
    const pr = (number: number, branch: string, title: string, state: PullRequestInfo['state']) => ({
      number,
      branch,
      title,
      url: `https://github.com/example/civet/pull/${number}`,
      state
    })
    return [
      pr(48, 'NO-TICKET/workspace-v3', 'Workspace v3 chrome', 'draft'),
      pr(47, 'quic-resume', 'Resume QUIC sessions across network changes', 'open'),
      pr(45, 'fix-flaky-e2e', 'Fix the flaky reconnect e2e', 'closed'),
      pr(44, 'scrollback-paging', 'Page scrollback over the control stream', 'merged')
    ]
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
  async onPorts(cb: (e: PortsEvent) => void) {
    return this.emitter.on('civet://ports', cb)
  }
  async onForwards(cb: (e: ForwardsEvent) => void) {
    return this.emitter.on('civet://forwards', cb)
  }

  async prepareFileDrop(_hostId: string, paths: string[]): Promise<DroppedFile[]> {
    return paths.map((source) => ({ source, path: source, error: null }))
  }
  async uploadFile(_hostId: string, name: string, _data: Uint8Array) {
    return `/tmp/civet-drops/mock/${name}`
  }
  // A browser has no native file paths to drop.
  async onFileDrop(_cb: (e: FileDropEvent) => void) {
    return () => {}
  }
}
