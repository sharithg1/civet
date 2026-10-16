// In-browser fake of the civet-desktop Rust core, auto-selected when the UI
// is not running inside Tauri (`pnpm dev` in a plain browser). It speaks the
// same CivetBackend interface and emits the same binary records as the real
// channel, so the record parser, frame batching, resize and scrollback paths
// all run for real against it.
//
// Fake host: repos "civet" and "orca" with a few worktrees, a folder project
// "notes" (not a git repository: file tabs only), an agent pane
// that finishes after a while, agents waiting for input / stopped with an
// error (to show every v3 status dot), and an echo shell per pane. Shell commands:
// help, seq N, status <running|needs|done|idle>, title T, drop, fail, replaced, exit.
// A host whose name contains "old" runs an older civetd with live panes (the
// "upgrade available" flow; Restart daemon starts a fresh one), "pre-git" runs
// a civetd that reports changes but takes no git actions, "fail" never
// connects, "udp-blocked" falls back to the SSH tunnel. Git actions in the
// Changes tab work on per-worktree fake state (MockGit). "Add project" adds a
// git repository for paths with a `src` directory in them, else a folder.

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
  PullRequestInfo,
  DirListing,
  FileEntry,
  FileList,
  FileOp,
  FileOpResult,
  FilePreview,
  HostDirListing,
  GitAction,
  GitBranchInfo,
  GitDiffInfo,
  GitDiscardSnapshot,
  GitFileChange,
  GitStatusInfo
} from './types'

/** This app's civetd version and the older one an "old" mock host runs. */
export const MOCK_VERSION = '0.1.18'
export const MOCK_OLD_VERSION = '0.1.1'
/** A civetd with git status but without git actions (before 0.1.14). */
export const MOCK_PRE_GIT_ACTIONS_VERSION = '0.1.13'
/** Mock daemons have folder projects from this version on (PROJECTS_MIN_DAEMON_VERSION). */
const MOCK_PROJECTS_VERSION = '0.1.17'
/** Mock daemons list host directories from this version on (like LIST_DIRS_MIN_DAEMON_VERSION). */
const MOCK_LIST_DIRS_VERSION = '0.1.18'

/** Numeric `a.b.c` comparison (state/fileops.ts imports the store, so not reused here). */
function versionAtLeast(version: string, min: string): boolean {
  const a = version.split('.').map(Number)
  const b = min.split('.').map(Number)
  for (let i = 0; i < 3; i++) if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) > (b[i] ?? 0)
  return true
}
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
    for (let n = historyStart; n < SYNTHETIC_HISTORY; n++) replay += `${historyLineVt(n)}\r\n`
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
        payload: `\x1bc${replay}${this.transcript}`
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
        echo += `^C\r\n${this.prompt}`
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
        this.host.backend.simulateFailure(
          this.host.summary.hostId,
          'connection lost: no route to host (mock)'
        )
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
      {
        id: 'civet',
        name: 'civet',
        path: '/home/dev/src/civet',
        defaultBranch: 'main',
        kind: 'git'
      },
      { id: 'orca', name: 'orca', path: '/home/dev/src/orca', defaultBranch: 'main', kind: 'git' },
      { id: 'notes', name: 'notes', path: '/home/dev/notes', defaultBranch: null, kind: 'folder' }
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
    this.worktrees.set('notes', [mockFolderRoot('notes', '/home/dev/notes')])
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
    this.addPane({
      worktree: v3.id,
      cwd: v3.path,
      title: 'Draft the PR description',
      agent: 'claude',
      status: 'idle'
    })
    this.addPane({
      worktree: v3.id,
      cwd: v3.path,
      title: 'Review the tab layout',
      agent: 'codex',
      command: ['codex'],
      status: 'idle'
    })
    this.addPane({ worktree: v3.id, cwd: v3.path, title: 'zsh', agent: null })
    // An agent that stopped with an error.
    const fl = this.worktrees.get('civet')![4]!
    const failed = this.addPane({
      worktree: fl.id,
      cwd: fl.path,
      title: 'Fix the flaky reconnect e2e',
      agent: 'claude',
      status: 'idle'
    })
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

  /** The folder project whose root is `worktree`, if any. */
  folderOf(worktree: string | null): RepoInfo | undefined {
    const wt = this.allWorktrees().find((w) => w.id === worktree)
    return this.repos.find((r) => r.id === wt?.repo && r.kind === 'folder')
  }

  /** `unsupported` like civetd: a folder project has no git. */
  checkGit(worktree: string, what = 'git'): void {
    const folder = this.folderOf(worktree)
    if (folder) {
      throw new CivetError(
        'unsupported',
        `unsupported: ${folder.name} is a folder, not a git repository: no ${what}`
      )
    }
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

/** Every mock worktree has these files; `target/` and `*.log` are gitignored. */
const MOCK_FILES: Record<string, string> = {
  '.gitignore': 'target/\n*.log\n',
  'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\nresolver = "3"\n',
  'README.md': [
    '# Civet',
    '',
    'Terminals and agents on remote hosts, from your Mac. See [the interfaces](docs/INTERFACES.md#crates) and [Usage](#usage).',
    '',
    '![Civet logo](docs/logo.png)',
    '',
    '## Usage',
    '',
    '- Add a project, then open a worktree',
    '- Run `civet handoff` from a pane',
    '  - nested item with **bold** and ~~strike~~',
    '',
    '- [x] Markdown preview',
    '- [ ] Editing',
    '',
    '> Agents run on the host; the app only draws them.',
    '',
    '| Crate | Role |',
    '| --- | --- |',
    '| civetd | daemon |',
    '| civet-proto | wire protocol |',
    '',
    '```sh',
    'cargo build --release',
    '```',
    '',
    'More at https://github.com.',
    ''
  ].join('\n'),
  'crates/civet-proto/Cargo.toml': '[package]\nname = "civet-proto"\n',
  'crates/civet-proto/src/lib.rs': '//! Wire protocol.\n\npub mod control;\npub mod frame;\n',
  'crates/civet-proto/src/control.rs':
    '//! Control RPC.\n\npub enum Request {\n    Ping { nonce: u64 },\n}\n',
  'crates/civet-proto/src/frame.rs':
    '//! Length-prefixed framing.\n\npub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;\n',
  'crates/civetd/src/main.rs': 'fn main() {\n    println!("civetd");\n}\n',
  'docs/INTERFACES.md':
    '# Civet crate interfaces\n\n## Crates\n\nBack to the [README](../README.md).\n',
  'docs/logo.png': '\u0089PNG\u0000\u0000',
  'scripts/release.sh': '#!/usr/bin/env bash\nset -euo pipefail\n',
  'debug.log': 'connected\n',
  'target/debug/civetd': '\u0000ELF'
}

/** A folder project's files (nothing is gitignored there). */
const MOCK_FOLDER_FILES: Record<string, string> = {
  'README.md': '# Notes\n\nPlain files, no git: open them from the Files tab.\n',
  'meetings/2026-10-14.md': '# Standup\n\n- QUIC resume landed\n- Folder projects next\n',
  'meetings/2026-10-15.md': '# Planning\n\n- Markdown preview\n',
  'todo.txt': 'ship folder projects\nwrite the release notes\n',
  'debug.log': 'not ignored here\n'
}

/** The only worktree of a folder project: its root. */
function mockFolderRoot(repo: string, path: string): WorktreeInfo {
  return { id: path, repo, path, branch: null, head: null, isMain: true, locked: false }
}

/** Mock daemons support file ops from this version on (like FILE_OPS_MIN_DAEMON_VERSION). */
const MOCK_FILE_OPS_VERSION = '0.1.15'

/** Every mock host's home directory. */
export const MOCK_HOME = '/home/dev'

/**
 * Directories on every mock host (for `listDirs`), relative to the home
 * directory; `*` marks a git repository. Added repos and every ancestor
 * exist too.
 */
const MOCK_HOST_DIRS = [
  '.cache',
  '.config',
  'Desktop',
  'Documents/notes',
  'Downloads',
  'go',
  'mirrors/linux.git*',
  'src/civet*',
  'src/civet/crates',
  'src/civet/docs',
  'src/civet-worktrees/quic-resume*',
  'src/civet-worktrees/scrollback-paging*',
  'src/civet-worktrees/fix-flaky-e2e*',
  'src/dotfiles*',
  'src/experiments/raft-sim*',
  'src/experiments/scratch',
  'src/orca*',
  'src/orca-worktrees/fix-wake-reconnect*',
  'src/website*'
]

/**
 * A worktree's files in memory: file bodies by path plus the set of
 * directories (so empty ones exist). Same rules as the daemon: relative
 * paths only, no `.git`, nothing overwritten.
 */
export class MockFs {
  readonly files = new Map<string, string>()
  readonly dirs = new Set<string>()

  static seeded(bodies: Record<string, string>): MockFs {
    const fs = new MockFs()
    for (const [path, body] of Object.entries(bodies)) {
      fs.files.set(path, body)
      for (let d = mockParent(path); d; d = mockParent(d)) fs.dirs.add(d)
    }
    return fs
  }

  /** `path` normalized, or `invalidArgument`. */
  static check(path: string): string {
    const parts = path.split('/').filter((p) => p && p !== '.')
    if (path.startsWith('/') || parts.includes('..'))
      throw new CivetError('invalidArgument', `"${path}" is outside the worktree`)
    if (parts.some((p) => p.toLowerCase() === '.git'))
      throw new CivetError('invalidArgument', `"${path}" is in .git, which is off limits`)
    if (!parts.length) throw new CivetError('invalidArgument', `"${path}" is the worktree root`)
    return parts.join('/')
  }

  exists(path: string): boolean {
    return this.files.has(path) || this.dirs.has(path)
  }

  /** Entries directly in `dir`. */
  children(dir: string): { name: string; path: string; dir: boolean }[] {
    const prefix = dir ? `${dir}/` : ''
    const out: { name: string; path: string; dir: boolean }[] = []
    for (const [set, isDir] of [
      [this.dirs, true],
      [this.files.keys(), false]
    ] as const) {
      for (const p of set) {
        if (p.startsWith(prefix) && !p.slice(prefix.length).includes('/'))
          out.push({ name: p.slice(prefix.length), path: p, dir: isDir })
      }
    }
    return out
  }

  /** A fresh path whose parent is a directory. */
  private fresh(path: string): string {
    const p = MockFs.check(path)
    const parent = mockParent(p)
    if (parent && !this.dirs.has(parent)) {
      if (this.files.has(parent))
        throw new CivetError('invalidArgument', `"${parent}" is not a directory`)
      throw new CivetError('notFound', `"${parent}" not found`)
    }
    if (this.exists(p)) throw new CivetError('alreadyExists', `already exists: "${p}"`)
    return p
  }

  private existing(path: string): string {
    const p = MockFs.check(path)
    if (!this.exists(p)) throw new CivetError('notFound', `"${p}" not found`)
    return p
  }

  /** A file (`body`) or, with null, a directory. */
  create(path: string, body: string | null): string {
    const p = this.fresh(path)
    if (body == null) this.dirs.add(p)
    else this.files.set(p, body)
    return p
  }

  /** Remove `path` and everything under it, as a tree rooted at ''. */
  take(path: string): MockFs {
    const p = this.existing(path)
    const out = new MockFs()
    const rel = (q: string) => (q === p ? '' : q.slice(p.length + 1))
    const under = (q: string) => q === p || q.startsWith(`${p}/`)
    for (const [q, body] of [...this.files]) {
      if (under(q)) {
        out.files.set(rel(q), body)
        this.files.delete(q)
      }
    }
    for (const q of [...this.dirs]) {
      if (under(q)) {
        out.dirs.add(rel(q))
        this.dirs.delete(q)
      }
    }
    return out
  }

  /** Put a tree from `take` at a fresh `path`. */
  put(path: string, tree: MockFs): string {
    const p = this.fresh(path)
    const at = (q: string) => (q ? `${p}/${q}` : p)
    for (const [q, body] of tree.files) this.files.set(at(q), body)
    for (const q of tree.dirs) this.dirs.add(at(q))
    return p
  }

  private snapshot(p: string): MockFs {
    const tree = this.take(p)
    this.put(p, tree)
    return tree
  }

  move(from: string, to: string): string {
    const src = this.existing(from)
    const dst = MockFs.check(to)
    if (dst.startsWith(`${src}/`))
      throw new CivetError('invalidArgument', `"${src}" cannot go inside itself`)
    this.fresh(dst)
    return this.put(dst, this.take(src))
  }

  copy(from: string, to: string): string {
    const src = this.existing(from)
    const dst = MockFs.check(to)
    if (dst.startsWith(`${src}/`))
      throw new CivetError('invalidArgument', `"${src}" cannot go inside itself`)
    return this.put(dst, this.snapshot(src))
  }
}

function mockParent(path: string): string {
  const i = path.lastIndexOf('/')
  return i < 0 ? '' : path.slice(0, i)
}

const mockIgnored = (path: string) =>
  path.startsWith('target/') || path === 'target' || path.endsWith('.log')

export class MockBackend implements CivetBackend {
  /** Status polls so far: the agent worktree's change grows with them. */
  private gitPolls = 0
  /** wsKey-like `host\0worktree` → fake git state. */
  private gits = new Map<string, MockGit>()
  readonly kind = 'mock' as const
  readonly emitter = new Emitter()
  private hosts = new Map<string, MockHost>()
  /** hostId → remote port → forward */
  private forwards = new Map<string, Map<number, ForwardView>>()
  private lastPorts = new Map<string, string>()

  constructor() {
    this.emitter.on<DaemonEventEnvelope>('civet://daemon-event', ({ hostId }) =>
      this.refreshPorts(hostId)
    )
    Object.assign(globalThis, {
      __civetMockFileDrop: (e: FileDropEvent) => this.simulateFileDrop(e)
    })
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
    const t =
      this.targets.get(hostId) ??
      (hostId === 'local'
        ? { target: { kind: 'local' } as const, transport: 'auto' as const }
        : null)
    if (!t) throw new CivetError('noSuchHost', `no host "${hostId}" (connect first)`)
    return this.connect(t.target, t.transport, false)
  }

  async restartDaemon(hostId: string) {
    const t =
      this.targets.get(hostId) ??
      (hostId === 'local'
        ? { target: { kind: 'local' } as const, transport: 'auto' as const }
        : null)
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
      daemonVersion: old
        ? MOCK_OLD_VERSION
        : hostId.includes('pre-git')
          ? MOCK_PRE_GIT_ACTIONS_VERSION
          : MOCK_VERSION,
      instanceId: replace ? '17179869185' : '17179869184',
      upgrade: old
        ? {
            runningVersion: MOCK_OLD_VERSION,
            availableVersion: MOCK_VERSION,
            livePanes: host.panes.size
          }
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
    if (h?.summary.state !== 'connected') {
      return { hostId, status: 'pending', message: null, ports: [] }
    }
    if (h.summary.transport === 'stream') {
      return {
        hostId,
        status: 'unsupported',
        message:
          'port forwarding needs the QUIC transport (this host is connected over the SSH tunnel)',
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
        command: `node node_modules/.bin/vite --port ${5173 + i}`,
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
    const forwards = [...this.forwardsOf(hostId).values()].sort(
      (a, b) => a.remotePort - b.remotePort
    )
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
        throw new CivetError(
          'alreadyExists',
          `port ${remotePort} is already forwarded to localhost:${existing.localPort}`
        )
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

  async openWebUrl(url: string) {
    window.open(url, '_blank', 'noopener')
  }

  async disconnectHost(hostId: string) {
    this.host(hostId)
    this.hostState(hostId, { state: 'disconnected', transport: null, rttMs: null, message: null })
    this.hosts.delete(hostId)
    this.lastPorts.delete(hostId)
    if (this.forwards.delete(hostId))
      this.emitter.emit<ForwardsEvent>('civet://forwards', { hostId, forwards: [] })
  }

  async listHosts() {
    return [...this.hosts.values()].map((h) => h.summary)
  }

  async notifyNetworkChange() {}

  /** Dev helper: the connection failed for good (needs a user Reconnect). */
  simulateFailure(hostId: string, message: string): void {
    if (!this.hosts.has(hostId)) return
    setTimeout(
      () => this.hostState(hostId, { state: 'failed', transport: null, rttMs: null, message }),
      300
    )
  }

  /** Dev helper: mimic a stall the Rust core would report after its 1 s threshold. */
  simulateNetworkDrop(hostId: string, outageMs = 2500): void {
    const h = this.hosts.get(hostId)
    if (!h) return
    const transport = h.summary.transport
    setTimeout(
      () =>
        this.hostState(hostId, { state: 'reconnecting', transport, rttMs: null, message: null }),
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

  /** The daemon has folder projects (ListProjects / AddProject). */
  private hasProjects(hostId: string): boolean {
    const v = this.host(hostId).summary.daemonVersion
    return !v || versionAtLeast(v, MOCK_PROJECTS_VERSION)
  }

  async listRepos(hostId: string) {
    const repos = [...this.host(hostId).repos]
    // An older daemon only answers ListRepos, which leaves folders out.
    return this.hasProjects(hostId) ? repos : repos.filter((r) => r.kind !== 'folder')
  }

  async addRepo(hostId: string, path: string) {
    const h = this.host(hostId)
    const name = path.replace(/\/+$/, '').split('/').pop() || path
    if (h.repos.some((r) => r.path === path)) {
      throw new CivetError('alreadyExists', `repo ${path} already added`)
    }
    if (!/(^|\/)src\//.test(path)) {
      if (!this.hasProjects(hostId)) {
        const v = h.summary.daemonVersion
        throw new CivetError(
          'invalidArgument',
          `invalid argument: ${path} is not a git repository. civetd ${v} on ${hostId} only adds git repositories; v${MOCK_PROJECTS_VERSION} or newer also adds plain folders (restart the daemon to upgrade)`
        )
      }
      const folder: RepoInfo = { id: name, name, path, defaultBranch: null, kind: 'folder' }
      h.repos.push(folder)
      h.worktrees.set(folder.id, [mockFolderRoot(folder.id, path)])
      h.event('reposChanged')
      return folder
    }
    const repo: RepoInfo = { id: name, name, path, defaultBranch: 'main', kind: 'git' }
    h.repos.push(repo)
    h.worktrees.set(repo.id, [
      {
        id: path,
        repo: repo.id,
        path,
        branch: 'main',
        head: 'f00dbab',
        isMain: true,
        locked: false
      }
    ])
    h.event('reposChanged')
    return repo
  }

  async listDirs(hostId: string, path: string): Promise<HostDirListing> {
    const v = this.host(hostId).summary.daemonVersion
    if (v && !versionAtLeast(v, MOCK_LIST_DIRS_VERSION))
      throw new CivetError(
        'unsupported',
        `${hostId} runs civetd ${v}; this needs v${MOCK_LIST_DIRS_VERSION} or newer (restart the daemon to upgrade)`
      )
    await sleep(40)
    const p = path.trim()
    const abs =
      p === '~' || p === ''
        ? MOCK_HOME
        : p.startsWith('~/')
          ? `${MOCK_HOME}/${p.slice(2)}`
          : p.startsWith('/')
            ? p
            : `${MOCK_HOME}/${p}`
    const dir = abs.replace(/\/+$/, '')
    const all = new Map<string, boolean>()
    for (const d of MOCK_HOST_DIRS) all.set(`${MOCK_HOME}/${d.replace(/\*$/, '')}`, d.endsWith('*'))
    for (const r of this.host(hostId).repos) all.set(r.path.replace(/\/+$/, ''), true)
    for (const d of [...all.keys()]) {
      for (let i = d.lastIndexOf('/'); i > 0; i = d.lastIndexOf('/', i - 1)) {
        const up = d.slice(0, i)
        if (!all.has(up)) all.set(up, false)
      }
    }
    if (dir && !all.has(dir)) throw new CivetError('notFound', `"${abs}" not found`)
    const dirs = [...all]
      .filter(([d]) => d.startsWith(`${dir}/`) && !d.slice(dir.length + 1).includes('/'))
      .map(([d, gitRepo]) => ({ name: d.slice(dir.length + 1), gitRepo }))
      .sort((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()))
    return { path: abs, dirs, truncated: false }
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

  async gitStatus(hostId: string, worktree: string): Promise<GitStatusInfo> {
    this.checkGitSupport(hostId)
    this.host(hostId).checkGit(worktree, 'changes')
    return this.git(hostId, worktree).status(this.gitPolls++)
  }

  async gitBranch(hostId: string, worktree: string): Promise<GitBranchInfo> {
    this.checkGitActions(hostId)
    this.host(hostId).checkGit(worktree, 'branch')
    return this.git(hostId, worktree).branchInfo()
  }

  async gitAction(
    hostId: string,
    worktree: string,
    action: GitAction
  ): Promise<GitDiscardSnapshot | null> {
    this.checkGitActions(hostId)
    this.host(hostId).checkGit(worktree, 'git actions')
    // Network actions take a moment, so the busy state shows.
    await sleep(typeof action === 'string' && ['push', 'pull', 'fetch'].includes(action) ? 600 : 80)
    return this.git(hostId, worktree).apply(action)
  }

  private git(hostId: string, worktree: string): MockGit {
    const key = `${hostId}\u0000${worktree}`
    let g = this.gits.get(key)
    if (!g) this.gits.set(key, (g = new MockGit(worktree, this.gitPolls)))
    return g
  }

  private checkGitActions(hostId: string): void {
    this.checkGitSupport(hostId)
    if (this.host(hostId).summary.daemonVersion === MOCK_PRE_GIT_ACTIONS_VERSION) {
      throw new CivetError(
        'unsupported',
        `${hostId} runs civetd ${MOCK_PRE_GIT_ACTIONS_VERSION}; this needs v0.1.14 or newer (restart the daemon to upgrade)`
      )
    }
  }

  async gitDiff(
    hostId: string,
    worktree: string,
    path: string,
    opts: { oldPath?: string | null; staged: boolean; contextLines?: number }
  ): Promise<GitDiffInfo> {
    this.checkGitSupport(hostId)
    this.host(hostId).checkGit(worktree, 'diffs')
    return mockGitDiff(worktree, path, opts.staged, this.gitPolls)
  }

  private checkGitSupport(hostId: string): void {
    if (this.host(hostId).summary.daemonVersion === MOCK_OLD_VERSION) {
      throw new CivetError(
        'unsupported',
        `the daemon on ${hostId} (${MOCK_OLD_VERSION}) is too old; restart it to upgrade`
      )
    }
  }

  async listPullRequests(hostId: string, repo: string): Promise<PullRequestInfo[]> {
    const main = this.host(hostId).worktrees.get(repo)?.[0]
    if (main) this.host(hostId).checkGit(main.id, 'pull requests')
    if (repo !== 'civet') return []
    const pr = (
      number: number,
      branch: string,
      title: string,
      state: PullRequestInfo['state']
    ) => ({
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
    if (r.kind === 'folder') {
      throw new CivetError(
        'unsupported',
        `unsupported: ${r.name} is a folder, not a git repository: no worktrees`
      )
    }
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

  private checkFiles(hostId: string, min = '0.1.10'): void {
    const v = this.host(hostId).summary.daemonVersion
    if (v === MOCK_OLD_VERSION)
      throw new CivetError(
        'unsupported',
        `${hostId} runs civetd ${v}; this needs v${min} or newer (restart the daemon to upgrade)`
      )
  }

  /** Each worktree's files, seeded from MOCK_FILES on first use. */
  private fs = new Map<string, MockFs>()
  private trash = new Map<string, { key: string; path: string; files: MockFs }>()
  private trashSeq = 0

  /** The worktree's files (exported for tests). */
  files(hostId: string, worktree: string): MockFs {
    const key = `${hostId}\u0000${worktree}`
    let fs = this.fs.get(key)
    const seed = this.host(hostId).folderOf(worktree) ? MOCK_FOLDER_FILES : MOCK_FILES
    if (!fs) this.fs.set(key, (fs = MockFs.seeded(seed)))
    return fs
  }

  async listFiles(
    hostId: string,
    worktree: string,
    dir: string,
    includeIgnored: boolean
  ): Promise<DirListing> {
    this.checkFiles(hostId)
    if (dir.split('/').includes('..') || dir.startsWith('/'))
      throw new CivetError('invalidArgument', `"${dir}" is outside the worktree`)
    const fs = this.files(hostId, worktree)
    const folder = !!this.host(hostId).folderOf(worktree)
    if (dir && !fs.dirs.has(dir)) {
      if (fs.files.has(dir)) throw new CivetError('invalidArgument', `"${dir}" is not a directory`)
      throw new CivetError('notFound', `"${dir}" not found`)
    }
    const entries = fs
      .children(dir)
      .map(({ name, path, dir: isDir }): FileEntry => {
        const body = fs.files.get(path) ?? ''
        return {
          name,
          kind: isDir ? 'dir' : 'file',
          size: isDir ? 0 : body.length,
          ignored: !folder && mockIgnored(path)
        }
      })
      .filter((e) => includeIgnored || !e.ignored)
      .sort(
        (a, b) =>
          Number(a.kind !== 'dir') - Number(b.kind !== 'dir') ||
          a.name.toLowerCase().localeCompare(b.name.toLowerCase())
      )
    return { entries, truncated: false }
  }

  async readFile(
    hostId: string,
    worktree: string,
    path: string,
    maxBytes: number
  ): Promise<FilePreview> {
    this.checkFiles(hostId)
    const body = this.files(hostId, worktree).files.get(path)
    if (body == null) throw new CivetError('notFound', `"${path}" not found`)
    const binary = body.includes('\u0000')
    const cap = Math.min(maxBytes, 1024 * 1024)
    return {
      size: body.length,
      binary,
      truncated: body.length > cap,
      text: binary ? '' : body.slice(0, cap)
    }
  }

  async listAllFiles(hostId: string, worktree: string): Promise<FileList> {
    this.checkFiles(hostId)
    const folder = !!this.host(hostId).folderOf(worktree)
    return {
      paths: [...this.files(hostId, worktree).files.keys()]
        .filter((p) => folder || !mockIgnored(p))
        .sort(),
      truncated: false
    }
  }

  async fileOp(hostId: string, worktree: string, op: FileOp): Promise<FileOpResult> {
    this.checkFiles(hostId, MOCK_FILE_OPS_VERSION)
    await sleep(60)
    const fs = this.files(hostId, worktree)
    const done = (path: string): FileOpResult => ({ path, trashId: null })
    if ('createFile' in op) return done(fs.create(op.createFile.path, ''))
    if ('createDir' in op) return done(fs.create(op.createDir.path, null))
    if ('move' in op) return done(fs.move(op.move.from, op.move.to))
    if ('copy' in op) return done(fs.copy(op.copy.from, op.copy.to))
    if ('trash' in op) {
      const path = MockFs.check(op.trash.path)
      const files = fs.take(path)
      const trashId = `${Date.now()}-1-${this.trashSeq++}`
      this.trash.set(trashId, { key: `${hostId}\u0000${worktree}`, path, files })
      return { path, trashId }
    }
    const t = this.trash.get(op.restore.trashId)
    if (!t || t.key !== `${hostId}\u0000${worktree}`)
      throw new CivetError('notFound', `trash entry "${op.restore.trashId}" not found`)
    fs.put(t.path, t.files)
    this.trash.delete(op.restore.trashId)
    return done(t.path)
  }

  async uploadToWorktree(
    hostId: string,
    worktree: string,
    dir: string,
    paths: string[]
  ): Promise<DroppedFile[]> {
    this.checkFiles(hostId, MOCK_FILE_OPS_VERSION)
    const fs = this.files(hostId, worktree)
    return paths.map((source) => {
      const name = source.slice(source.lastIndexOf('/') + 1)
      const path = dir ? `${dir}/${name}` : name
      try {
        fs.create(path, `uploaded from ${source}\n`)
        return { source, path, error: null }
      } catch (err) {
        return { source, path: null, error: CivetError.from(err).message }
      }
    })
  }

  async downloadFile(hostId: string, worktree: string, path: string): Promise<string | null> {
    this.checkFiles(hostId, MOCK_FILE_OPS_VERSION)
    const body = this.files(hostId, worktree).files.get(path)
    if (body == null) throw new CivetError('notFound', `"${path}" not found`)
    const name = path.slice(path.lastIndexOf('/') + 1)
    // A browser download stands in for the save dialog.
    if (typeof document !== 'undefined' && typeof URL.createObjectURL === 'function') {
      const a = document.createElement('a')
      a.href = URL.createObjectURL(new Blob([body]))
      a.download = name
      a.click()
      URL.revokeObjectURL(a.href)
    }
    return `~/Downloads/${name}`
  }

  async listPanes(hostId: string) {
    return [...this.host(hostId).panes.values()].map((p) => p.info)
  }

  async spawnPane(hostId: string, request: SpawnPaneRequest) {
    const h = this.host(hostId)
    const folder = h.folderOf(request.worktree)
    if (folder) {
      throw new CivetError(
        'unsupported',
        `unsupported: ${folder.name} is a folder project: it has no sessions`
      )
    }
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
  // A browser has no native file paths to drop: `simulateFileDrop` (also
  // `globalThis.__civetMockFileDrop`, for the UI driver) stands in for Finder.
  async onFileDrop(cb: (e: FileDropEvent) => void) {
    return this.emitter.on('file-drop', cb)
  }
  simulateFileDrop(e: FileDropEvent): void {
    this.emitter.emit('file-drop', e)
  }
}

// ── Git status / diffs (right panel → Changes) ──────────────────────────────

const change = (
  path: string,
  kind: GitFileChange['kind'],
  additions: number | null,
  deletions: number | null,
  oldPath: string | null = null
): GitFileChange => ({ path, oldPath, kind, additions, deletions })

/** Lines of the file an "agent" keeps writing in non-main worktrees. */
const growingLines = (polls: number) => 4 + Math.min(40, polls)

function mockGitStatus(worktree: string): GitStatusInfo {
  const empty: GitStatusInfo = {
    staged: [],
    unstaged: [],
    untracked: [],
    conflicted: [],
    truncated: false
  }
  if (worktree.endsWith('/civet')) return empty
  if (worktree.endsWith('fix-flaky-e2e')) {
    return { ...empty, conflicted: [change('tests/e2e/src/reconnect.rs', 'conflicted', 6, 2)] }
  }
  return {
    ...empty,
    staged: [
      change(
        'crates/civet-transport/src/resume.rs',
        'renamed',
        3,
        1,
        'crates/civet-transport/src/rejoin.rs'
      ),
      change('docs/RESUME.md', 'added', 5, 0)
    ],
    unstaged: [
      change('crates/civet-transport/src/session.rs', 'modified', 4, 2),
      change('crates/civet-transport/src/legacy.rs', 'deleted', 0, 3),
      change('assets/logo.png', 'modified', null, null)
    ],
    untracked: [change('crates/civet-transport/tests/resume.rs', 'untracked', 4, 0)]
  }
}

const GROWING = 'crates/civet-transport/src/session.rs'
const fakeSha = () =>
  Array.from({ length: 40 }, () => Math.floor(Math.random() * 16).toString(16)).join('')
const gitErr = (message: string) => new CivetError('git', `git: ${message}`)

type Lists = Pick<GitStatusInfo, 'staged' | 'unstaged' | 'untracked' | 'conflicted'>
const cloneLists = (l: Lists): Lists => ({
  staged: [...l.staged],
  unstaged: [...l.unstaged],
  untracked: [...l.untracked],
  conflicted: [...l.conflicted]
})

/**
 * One worktree's fake git: the seeded changes of mockGitStatus, a branch with
 * an upstream (or none, like a fresh agent branch) and a stash, changed by
 * GitActions roughly the way git would.
 */
class MockGit {
  lists: Lists
  branch: GitBranchInfo
  /** Changes of each unpushed commit, newest last (what Undo puts back). */
  private commits: GitFileChange[][] = []
  private stash: Lists[] = []
  private discarded = new WeakMap<GitDiscardSnapshot, Lists>()
  /** The session.rs edit grows while the "agent" keeps writing (until touched). */
  private growing = true

  constructor(
    worktree: string,
    private readonly pollBase: number
  ) {
    const { truncated: _, ...lists } = mockGitStatus(worktree)
    this.lists = lists
    const name = worktree.split('/').pop() ?? 'main'
    const branch = name === 'civet' ? 'main' : name
    const published = name === 'civet' || name === 'quic-resume' || name === 'fix-flaky-e2e'
    this.branch = {
      branch,
      head: fakeSha(),
      upstream: published ? `origin/${branch}` : null,
      ahead: name === 'quic-resume' ? 2 : name === 'fix-flaky-e2e' ? 1 : 0,
      behind: name === 'fix-flaky-e2e' ? 1 : 0,
      canUndoCommit: name === 'quic-resume',
      stashes: 0,
      hasRemote: true
    }
    if (name === 'quic-resume') this.commits = [[], []]
  }

  status(polls: number): GitStatusInfo {
    const unstaged = this.lists.unstaged.map((c) =>
      c.path === GROWING && this.growing
        ? { ...c, additions: growingLines(polls - this.pollBase) }
        : c
    )
    return { ...cloneLists(this.lists), unstaged, truncated: false }
  }

  branchInfo(): GitBranchInfo {
    return { ...this.branch, stashes: this.stash.length }
  }

  apply(action: GitAction): GitDiscardSnapshot | null {
    if (typeof action === 'string') {
      this[action]()
      return null
    }
    if ('stage' in action) this.stage(action.stage.paths)
    else if ('unstage' in action) this.unstage(action.unstage.paths)
    else if ('discard' in action) return this.discard(action.discard.paths, action.discard.staged)
    else if ('restoreDiscarded' in action) this.restore(action.restoreDiscarded)
    else if ('commit' in action) this.commit(action.commit)
    else if ('acceptConflict' in action) this.stage(action.acceptConflict.paths)
    else if ('undoCommit' in action) this.undoCommit(action.undoCommit.head)
    return null
  }

  private take(side: keyof Lists, paths: string[]): GitFileChange[] {
    const set = new Set(paths)
    const hit = this.lists[side].filter((c) => set.has(c.path) || (c.oldPath && set.has(c.oldPath)))
    this.lists[side] = this.lists[side].filter((c) => !hit.includes(c))
    if (hit.some((c) => c.path === GROWING)) this.growing = false
    return hit
  }

  private put(side: keyof Lists, changes: GitFileChange[]): void {
    const paths = new Set(changes.map((c) => c.path))
    this.lists[side] = [...this.lists[side].filter((c) => !paths.has(c.path)), ...changes].sort(
      (a, b) => a.path.localeCompare(b.path)
    )
  }

  private stage(paths: string[]): void {
    this.put('staged', [
      ...this.take('unstaged', paths),
      ...this.take('untracked', paths).map((c) => ({ ...c, kind: 'added' as const })),
      ...this.take('conflicted', paths).map((c) => ({ ...c, kind: 'modified' as const }))
    ])
  }

  private unstage(paths: string[]): void {
    const back = this.take('staged', paths)
    this.put(
      'untracked',
      back.filter((c) => c.kind === 'added').map((c) => ({ ...c, kind: 'untracked' as const }))
    )
    this.put(
      'unstaged',
      back.filter((c) => c.kind !== 'added')
    )
  }

  private discard(paths: string[], staged: boolean): GitDiscardSnapshot {
    if (this.lists.conflicted.some((c) => paths.includes(c.path))) {
      throw new CivetError(
        'invalidArgument',
        'invalid argument: resolve conflicts instead of discarding'
      )
    }
    const before = cloneLists(this.lists)
    const gone = [
      ...this.take('unstaged', paths),
      ...this.take('untracked', paths),
      ...(staged ? this.take('staged', paths) : [])
    ]
    const snapshot: GitDiscardSnapshot = {
      entries: gone.map((c) => ({
        path: c.path,
        file: c.kind === 'deleted' ? null : { oid: fakeSha(), mode: 0o100644 },
        restoreIndex: staged,
        index: null,
        untracked: c.kind === 'untracked'
      }))
    }
    this.discarded.set(snapshot, before)
    return snapshot
  }

  private restore(snapshot: GitDiscardSnapshot): void {
    const before = this.discarded.get(snapshot)
    if (!before) throw gitErr('blob is gone')
    const paths = new Set(snapshot.entries.map((e) => e.path))
    for (const side of ['staged', 'unstaged', 'untracked'] as const) {
      this.put(
        side,
        before[side].filter((c) => paths.has(c.path))
      )
    }
  }

  private commit(opts: { message: string; amend: boolean; all: boolean }): void {
    if (!opts.message.trim() && !opts.amend) {
      throw new CivetError('invalidArgument', 'invalid argument: the commit message is empty')
    }
    if (opts.all) {
      this.stage([...this.lists.unstaged, ...this.lists.untracked].map((c) => c.path))
    }
    if (this.lists.staged.length === 0 && !opts.amend) {
      throw gitErr('no changes added to commit (use "git add" and/or "git commit -a")')
    }
    const changes = this.take(
      'staged',
      this.lists.staged.map((c) => c.path)
    )
    if (opts.amend && this.commits.length > 0)
      this.commits[this.commits.length - 1]!.push(...changes)
    else {
      this.commits.push(changes)
      this.branch.ahead += 1
    }
    this.branch.head = fakeSha()
    this.branch.canUndoCommit = true
  }

  private undoCommit(head: string): void {
    if (head !== this.branch.head)
      throw new CivetError('invalidArgument', 'invalid argument: HEAD moved; refresh and try again')
    if (!this.branch.canUndoCommit)
      throw new CivetError('invalidArgument', 'invalid argument: the last commit is already pushed')
    this.put('staged', this.commits.pop() ?? [])
    this.branch.ahead = Math.max(0, this.branch.ahead - 1)
    this.branch.head = fakeSha()
    this.branch.canUndoCommit = this.commits.length > 0 && this.branch.ahead > 0
  }

  private push(): void {
    if (this.branch.behind > 0) {
      throw gitErr(
        `To github.com:example/civet.git\n ! [rejected]        ${this.branch.branch} -> ${this.branch.branch} (fetch first)\nerror: failed to push some refs to 'github.com:example/civet.git'\nhint: Updates were rejected because the remote contains work that you do not\nhint: have locally.`
      )
    }
    this.branch.upstream = `origin/${this.branch.branch}`
    this.branch.ahead = 0
    this.branch.canUndoCommit = false
    this.commits = []
  }

  private pull(): void {
    if (!this.branch.upstream) {
      throw gitErr(`${this.branch.branch} has no upstream branch to pull from (push it first)`)
    }
    if (this.branch.behind > 0) {
      // Diverged: the merge is one more unpushed (empty) commit.
      if (this.branch.ahead > 0) {
        this.branch.ahead += 1
        this.commits.push([])
      }
      this.branch.behind = 0
      this.branch.head = fakeSha()
    }
  }

  private fetch(): void {
    // Someone else pushed to a published branch in the meantime.
    if (this.branch.upstream && this.branch.branch !== 'main') this.branch.behind += 1
  }

  private stashAll(): void {
    const l = this.lists
    if (!l.staged.length && !l.unstaged.length && !l.untracked.length) {
      throw gitErr('no local changes to stash')
    }
    this.stash.push({ ...cloneLists(l), conflicted: [] })
    this.growing = false
    this.lists = { staged: [], unstaged: [], untracked: [], conflicted: l.conflicted }
  }

  private stashPop(): void {
    const top = this.stash.pop()
    if (!top) throw gitErr('no stash entries for this branch')
    for (const side of ['staged', 'unstaged', 'untracked'] as const) this.put(side, top[side])
  }
}

function mockGitDiff(_worktree: string, path: string, staged: boolean, polls: number): GitDiffInfo {
  const base = { path, binary: false, tooLarge: false }
  const diff = (header: string, hunks: string, additions: number, deletions: number) => ({
    ...base,
    additions,
    deletions,
    diff: `diff --git a/${path} b/${path}\n${header}${hunks}`
  })
  if (path.endsWith('.png'))
    return { ...base, binary: true, additions: null, deletions: null, diff: '' }
  if (path.endsWith('reconnect.rs')) {
    return diff(
      `index 1111111,2222222..0000000\n--- a/${path}\n+++ b/${path}\n`,
      [
        '@@@ -10,4 -10,4 +10,10 @@@',
        '  fn reconnect_after_wake() {',
        '++<<<<<<< HEAD',
        ' +    let deadline = Duration::from_secs(5);',
        '++=======',
        '+     let deadline = Duration::from_secs(10);',
        '++>>>>>>> fix-flaky-e2e',
        '      wait_for(deadline);',
        '  }',
        ''
      ].join('\n'),
      6,
      2
    )
  }
  if (path.endsWith('session.rs') && !staged) {
    const n = growingLines(polls)
    const added = Array.from({ length: n }, (_, i) =>
      i === 0 ? '+    // Resume the session on the new path.' : `+    self.replay(${i}).await?;`
    )
    return diff(
      `index 3a4b5c6..7d8e9f0 100644\n--- a/${path}\n+++ b/${path}\n`,
      [
        `@@ -40,7 +40,${5 + n} @@ impl Session {`,
        '     pub async fn resume(&mut self) -> Result<(), Error> {',
        '-        let token = self.token.clone();',
        '-        self.reconnect(token).await',
        '+        let token = self.token.clone().ok_or(Error::NoToken)?;',
        '+        self.reconnect(token).await?;',
        ...added.slice(2),
        '+        Ok(())',
        '     }',
        ' ',
        '     fn on_path_change(&mut self) {',
        ''
      ].join('\n'),
      n,
      2
    )
  }
  if (path.endsWith('legacy.rs')) {
    return diff(
      `deleted file mode 100644\nindex 4444444..0000000\n--- a/${path}\n+++ /dev/null\n`,
      ['@@ -1,3 +0,0 @@', '-//! Old reconnect path.', '-', '-pub fn rejoin() {}', ''].join('\n'),
      0,
      3
    )
  }
  if (path.endsWith('resume.rs') && staged) {
    return diff(
      'similarity index 82%\nrename from crates/civet-transport/src/rejoin.rs\nrename to crates/civet-transport/src/resume.rs\n' +
        `index 5555555..6666666 100644\n--- a/crates/civet-transport/src/rejoin.rs\n+++ b/${path}\n`,
      [
        '@@ -1,6 +1,8 @@',
        '-//! Rejoin a session.',
        '+//! Resume a session after a network change.',
        '+//!',
        '+//! See docs/RESUME.md.',
        ' ',
        ' use crate::Session;',
        ' ',
        ' pub fn resume(s: &mut Session) {',
        ''
      ].join('\n'),
      3,
      1
    )
  }
  const lines = path.endsWith('.md')
    ? [
        '# Session resume',
        '',
        'A client keeps its session across',
        'network changes and sleep.',
        ''
      ]
    : [
        '#[tokio::test]',
        'async fn resumes() {',
        '    let s = connect().await;',
        '    s.resume().await.unwrap();'
      ]
  const n = lines.length
  const header = `new file mode 100644\nindex 0000000..7777777\n--- /dev/null\n+++ b/${path}\n`
  return diff(header, `@@ -0,0 +1,${n} @@\n${lines.map((l) => `+${l}`).join('\n')}\n`, n, 0)
}
