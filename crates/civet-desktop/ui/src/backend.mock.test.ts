import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { MOCK_VERSION, MockBackend } from './backend.mock'
import { PaneStream, type TerminalSink } from './terminal/pane-stream'
import { RecordKind, isFull, parseRecords, type TermRecord } from './terminal/records'
import type { HostStateEvent } from './types'

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

async function connected() {
  const b = new MockBackend()
  const states: HostStateEvent[] = []
  await b.onHostState((e) => states.push(e))
  const p = b.connectHost({ kind: 'ssh', destination: 'devbox' })
  await vi.runAllTimersAsync()
  return { b, summary: await p, states }
}

describe('MockBackend', () => {
  it('changes worktree files like the daemon: no overwrite, no .git, trash + restore', async () => {
    const { b } = await connected()
    const op = (o: Parameters<MockBackend['fileOp']>[2]) => {
      const p = b.fileOp('devbox', '/w', o)
      void vi.runAllTimersAsync()
      return p
    }
    const names = async (dir: string) =>
      (await b.listFiles('devbox', '/w', dir, true)).entries.map((e) => e.name)
    await op({ createDir: { path: 'notes' } })
    expect(await names('notes')).toEqual([])
    await op({ createFile: { path: 'notes/a.md' } })
    await op({ copy: { from: 'notes', to: 'notes2' } })
    await op({ move: { from: 'notes2/a.md', to: 'b.md' } })
    expect(await names('notes2')).toEqual([])
    expect(await names('')).toContain('b.md')
    await expect(op({ createFile: { path: 'README.md' } })).rejects.toMatchObject({
      code: 'alreadyExists'
    })
    await expect(op({ move: { from: 'notes', to: 'notes/x' } })).rejects.toMatchObject({
      code: 'invalidArgument'
    })
    for (const path of ['.git/config', '../x', '/etc', ''])
      await expect(op({ createFile: { path } })).rejects.toMatchObject({ code: 'invalidArgument' })

    const t = await op({ trash: { path: 'docs' } })
    expect(await names('')).not.toContain('docs')
    await op({ createDir: { path: 'docs' } })
    await expect(op({ restore: { trashId: t.trashId! } })).rejects.toMatchObject({
      code: 'alreadyExists'
    })
    await op({ trash: { path: 'docs' } })
    expect(await op({ restore: { trashId: t.trashId! } })).toMatchObject({ path: 'docs' })
    expect(await names('docs')).toEqual(['INTERFACES.md', 'logo.png'])
    // Other worktrees are untouched.
    expect((await b.listFiles('devbox', '/other', '', false)).entries.map((e) => e.name)).toContain(
      'README.md'
    )

    const up = await b.uploadToWorktree('devbox', '/w', 'docs', ['/tmp/shot.png', '/x/logo.png'])
    expect(up.map((u) => u.path)).toEqual(['docs/shot.png', null])
    expect(up[1]!.error).toMatch(/already exists/)
  })

  it('connects with progress and lists repos, worktrees and panes', async () => {
    const { b, summary, states } = await connected()
    expect(summary).toMatchObject({ hostId: 'devbox', state: 'connected', transport: 'quic' })
    expect(states.map((s) => s.state)).toEqual(['connecting', 'connected'])
    const repos = await b.listRepos('devbox')
    expect(repos.map((r) => r.id)).toEqual(['civet', 'orca', 'notes'])
    expect((await b.listWorktrees('devbox', 'civet')).length).toBe(5)
    const panes = await b.listPanes('devbox')
    expect(panes.length).toBe(7)
    expect(panes.map((p) => p.status)).toContain('needsInput')
    expect(panes.some((p) => p.exited && p.exitCode === 1)).toBe(true)
  })

  it('streams a Full then echo diffs through valid binary records', async () => {
    const { b } = await connected()
    const pane = (await b.listPanes('devbox')).find((p) => !p.agent)!
    const records: TermRecord[] = []
    const att = await b.attachPane('devbox', pane.id, (m) => records.push(...parseRecords(m)))
    await vi.advanceTimersByTimeAsync(10)
    expect(records[0]!.kind).toBe(RecordKind.Screen)
    expect(isFull(records[0]!)).toBe(true)
    await b.writeInput('devbox', pane.id, 'hi')
    await vi.advanceTimersByTimeAsync(10)
    const last = records.at(-1)!
    expect(new TextDecoder().decode(last.payload)).toBe('hi')
    expect(last.state).toBeGreaterThan(records[0]!.state)

    // Records drive a PaneStream / terminal sink end to end, including resize.
    const sink: TerminalSink & { cols: number; rows: number } = {
      cols: 1,
      rows: 1,
      resize(c, r) {
        this.cols = c
        this.rows = r
      },
      write() {}
    }
    const stream = new PaneStream()
    stream.apply(records, sink)
    expect([sink.cols, sink.rows]).toEqual([pane.size.cols, pane.size.rows])
    await b.resizePane('devbox', pane.id, { cols: 90, rows: 20, pixelWidth: 0, pixelHeight: 0 })
    await vi.advanceTimersByTimeAsync(10)
    stream.apply(records.slice(-1), sink)
    expect([sink.cols, sink.rows]).toEqual([90, 20])
    await att.detach()
  })

  it('serves scrollback pages and emits pane-closed on kill', async () => {
    const { b } = await connected()
    const pane = (await b.listPanes('devbox'))[1]!
    const page = b.fetchScrollback('devbox', pane.id, { before: { end: 1000, limit: 10 } })
    await vi.advanceTimersByTimeAsync(200)
    expect((await page).start).toBe(990)
    const kinds: number[] = []
    await b.attachPane('devbox', pane.id, (m) => kinds.push(...parseRecords(m).map((r) => r.kind)))
    await b.killPane('devbox', pane.id)
    await vi.advanceTimersByTimeAsync(10)
    expect(kinds).toContain(RecordKind.PaneClosed)
  })

  it('has folder projects: files without ignores and sessions, no git', async () => {
    const { b } = await connected()
    const notes = (await b.listRepos('devbox')).find((r) => r.id === 'notes')!
    expect(notes).toMatchObject({ kind: 'folder', defaultBranch: null })
    const [root] = await b.listWorktrees('devbox', 'notes')
    expect(root).toMatchObject({ isMain: true, branch: null, head: null, path: notes.path })
    const all = await b.listAllFiles('devbox', root!.id)
    expect(all.paths).toContain('debug.log')
    expect(all.paths).toContain('meetings/2026-10-14.md')
    const listing = await b.listFiles('devbox', root!.id, '', false)
    expect(listing.entries.every((e) => !e.ignored)).toBe(true)
    await expect(b.gitStatus('devbox', root!.id)).rejects.toMatchObject({ code: 'unsupported' })
    await expect(b.createWorktree('devbox', 'notes', 'x')).rejects.toMatchObject({
      code: 'unsupported'
    })
    expect(
      await b.spawnPane('devbox', {
        worktree: root!.id,
        cwd: null,
        command: null,
        env: [],
        size: { cols: 80, rows: 24, pixelWidth: 0, pixelHeight: 0 },
        title: null,
        agent: null
      })
    ).toMatchObject({ worktree: root!.id, cwd: root!.path })

    // Add project: a path with a src directory is a repository, else a folder.
    expect(await b.addRepo('devbox', '/home/dev/src/web')).toMatchObject({ kind: 'git' })
    const docs = await b.addRepo('devbox', '/home/dev/docs')
    expect(docs).toMatchObject({ kind: 'folder', name: 'docs' })
    expect(await b.listWorktrees('devbox', docs.id)).toEqual([
      expect.objectContaining({ id: '/home/dev/docs', isMain: true, branch: null })
    ])
  })

  it('an older daemon lists no folders and refuses to add one', async () => {
    const b = new MockBackend()
    const p = b.connectHost({ kind: 'ssh', destination: 'old-box' }, 'ssh')
    await vi.runAllTimersAsync()
    await p
    expect((await b.listRepos('old-box')).map((r) => r.id)).toEqual(['civet', 'orca'])
    await expect(b.addRepo('old-box', '/home/dev/docs')).rejects.toMatchObject({
      code: 'invalidArgument',
      message: expect.stringMatching(/not a git repository.*v0\.1\.17 or newer/)
    })
  })

  it('offers a daemon upgrade on an "old" host; restart_daemon replaces it', async () => {
    const b = new MockBackend()
    const states: HostStateEvent[] = []
    await b.onHostState((e) => states.push(e))
    const p = b.connectHost({ kind: 'ssh', destination: 'old-box', port: 2222 }, 'ssh')
    await vi.runAllTimersAsync()
    const s = await p
    expect(s.upgrade).toEqual({
      runningVersion: '0.1.1',
      availableVersion: MOCK_VERSION,
      livePanes: 7
    })
    expect(states.at(-1)!.upgrade).toEqual(s.upgrade)
    // Reconnect keeps the stored target and transport.
    b.simulateFailure('old-box', 'lost')
    await vi.advanceTimersByTimeAsync(400)
    expect(states.at(-1)!.state).toBe('failed')
    const r = b.reconnectHost('old-box')
    await vi.runAllTimersAsync()
    expect((await r).transport).toBe('stream')
    expect(b.targets.get('old-box')!.target).toMatchObject({ port: 2222 })
    const restarted = b.restartDaemon('old-box')
    await vi.runAllTimersAsync()
    const after = await restarted
    expect(after.upgrade).toBeNull()
    expect(after.daemonVersion).toBe(MOCK_VERSION)
    expect(after.instanceId).not.toBe(s.instanceId)
    expect(await b.listPanes('old-box')).toEqual([])
    await expect(b.reconnectHost('nope')).rejects.toMatchObject({ code: 'noSuchHost' })
  })

  it('only agent panes ask for attention', async () => {
    const { b } = await connected()
    const seen: number[] = []
    await b.onAgentAttention((e) => seen.push(e.pane))
    const shell = (await b.listPanes('devbox')).find((p) => !p.agent && !p.exited)!
    await b.writeInput('devbox', shell.id, 'exit\r')
    await vi.advanceTimersByTimeAsync(10)
    expect(seen).toEqual([])
  })

  it('reports sample ports and forwards with a remap when the local port is taken', async () => {
    const { b } = await connected()
    const ports = await b.listPorts('devbox')
    expect(ports.status).toBe('watching')
    expect(ports.ports.map((p) => p.port)).toEqual(expect.arrayContaining([3001, 5173, 5432]))
    const events: number[][] = []
    await b.onForwards((e) => events.push(e.forwards.map((f) => f.remotePort)))
    expect(await b.forwardPort('devbox', 5173)).toMatchObject({ remotePort: 5173, localPort: 5173 })
    // 3001 is "taken" locally in the mock: remapped.
    expect((await b.forwardPort('devbox', 3001)).localPort).toBe(51842)
    // Idempotent; a different explicit local port is refused.
    expect((await b.forwardPort('devbox', 3001)).localPort).toBe(51842)
    await expect(b.forwardPort('devbox', 3001, 9000)).rejects.toMatchObject({
      code: 'alreadyExists'
    })
    await b.stopForward('devbox', 5173)
    await expect(b.stopForward('devbox', 5173)).rejects.toMatchObject({ code: 'notFound' })
    expect(events.at(-1)).toEqual([3001])
  })

  it('rejects unknown hosts with a CivetError code', async () => {
    const b = new MockBackend()
    await expect(b.listRepos('nope')).rejects.toMatchObject({ code: 'noSuchHost' })
  })
})

describe('MockBackend files', () => {
  it('lists, previews and quick-opens, hiding ignored files by default', async () => {
    const { b } = await connected()
    const root = await b.listFiles('devbox', '/w', '', false)
    expect(root.entries.map((e) => e.name)).not.toContain('target')
    const all = await b.listFiles('devbox', '/w', '', true)
    expect(all.entries.filter((e) => e.ignored).map((e) => e.name)).toEqual(['target', 'debug.log'])
    expect((await b.readFile('devbox', '/w', 'README.md', 1024)).text).toContain('Civet')
    expect((await b.readFile('devbox', '/w', 'docs/logo.png', 1024)).binary).toBe(true)
    await expect(b.listFiles('devbox', '/w', '../x', false)).rejects.toMatchObject({
      code: 'invalidArgument'
    })
    expect((await b.listAllFiles('devbox', '/w')).paths).toContain('crates/civetd/src/main.rs')
  })
})

describe('MockBackend folders', () => {
  it('lists any host directory with git repos flagged, added repos included', async () => {
    const { b } = await connected()
    const src = b.listDirs('devbox', '~/src/')
    await vi.runAllTimersAsync()
    const l = await src
    expect(l.path).toBe('/home/dev/src/')
    expect(l.dirs).toContainEqual({ name: 'civet', gitRepo: true })
    expect(l.dirs).toContainEqual({ name: 'experiments', gitRepo: false })
    const home = b.listDirs('devbox', '~')
    await vi.runAllTimersAsync()
    expect((await home).dirs.map((d) => d.name)).toContain('.config')
    const root = b.listDirs('devbox', '/')
    await vi.runAllTimersAsync()
    expect((await root).dirs).toEqual([{ name: 'home', gitRepo: false }])

    await b.addRepo('devbox', '/srv/app')
    const srv = b.listDirs('devbox', '/srv')
    await vi.runAllTimersAsync()
    expect((await srv).dirs).toEqual([{ name: 'app', gitRepo: true }])

    const missing = b.listDirs('devbox', '~/nope')
    const failed = expect(missing).rejects.toMatchObject({ code: 'notFound' })
    await vi.runAllTimersAsync()
    await failed
  })

  it('is unsupported before civetd 0.1.18', async () => {
    const b = new MockBackend()
    const p = b.connectHost({ kind: 'ssh', destination: 'pre-git' })
    await vi.runAllTimersAsync()
    await p
    await expect(b.listDirs('pre-git', '~')).rejects.toMatchObject({ code: 'unsupported' })
  })
})

describe('MockBackend host stats', () => {
  it('serves a history, then only newer samples, and processes on request', async () => {
    const { b } = await connected()
    const stats = async (since: number | null, processes: boolean) => {
      const p = b.hostStats('devbox', since, processes)
      await vi.advanceTimersByTimeAsync(50)
      return p
    }
    const first = await stats(null, false)
    expect(first.samples.length).toBeGreaterThan(100)
    expect(first.processes).toEqual([])
    const newest = first.samples.at(-1)!.seq
    await vi.advanceTimersByTimeAsync(3950)
    const next = await stats(newest, true)
    expect(next.samples.map((s) => s.seq)).toEqual([newest + 1, newest + 2])
    expect(next.processes.some((p) => p.pane != null && p.worktree != null)).toBe(true)
    expect(next.processes.at(-1)!.pane).toBeNull()
  })

  it('is unsupported before civetd 0.2.1', async () => {
    const b = new MockBackend()
    const p = b.connectHost({ kind: 'ssh', destination: 'pre-git' })
    await vi.runAllTimersAsync()
    await p
    await expect(b.hostStats('pre-git', null, false)).rejects.toMatchObject({ code: 'unsupported' })
  })
})

describe('MockBackend git', () => {
  it('reports changes and diffs', async () => {
    const { b } = await connected()
    const wt = (await b.listWorktrees('devbox', 'civet'))[1]!
    const s = await b.gitStatus('devbox', wt.id)
    expect(s.unstaged.length).toBeGreaterThan(0)
    const file = s.unstaged.find((c) => c.kind === 'modified' && c.additions != null)!
    const d = await b.gitDiff('devbox', wt.id, file.path, { staged: false })
    expect(d.diff).toContain('@@')
    const bin = s.unstaged.find((c) => c.additions == null)!
    expect((await b.gitDiff('devbox', wt.id, bin.path, { staged: false })).binary).toBe(true)
  })
})

describe('MockBackend agent activity', () => {
  it('has a seeded timeline and records turns as agents run', async () => {
    const { b } = await connected()
    const [, quic, scroll, v3] = await b.listWorktrees('devbox', 'civet')
    const turns = async (wt: string) => (await b.agentActivity('devbox', wt)).turns
    const v3Turns = await turns(v3!.id)
    expect(v3Turns.map((t) => t.status)).toEqual(['done', 'done', 'idle', 'needsInput'])
    // Oldest first.
    expect(v3Turns.map((t) => t.startedMs)).toEqual([...v3Turns.map((t) => t.startedMs)].sort())
    const edit = v3Turns[0]!
    const file = edit.files.find((f) => f.kind === 'added')!
    const d = await b.turnDiff('devbox', v3!.id, file.path, {
      from: edit.fromTree!,
      to: edit.toTree!
    })
    expect(d.diff).toContain('@@')

    // The running agent finished (while connecting) with the worktree's changes.
    const done = (await turns(quic!.id)).at(-1)!
    expect(done).toMatchObject({ pane: 1, status: 'done', prompt: 'Implement QUIC resume' })
    expect(done.files.length).toBeGreaterThan(0)

    // A new agent pane in a quiet worktree: running, then needs input.
    expect(await turns(scroll!.id)).toEqual([])
    const pane = await b.spawnPane('devbox', {
      worktree: scroll!.id,
      cwd: null,
      command: { program: 'claude', args: [] },
      env: [],
      size: { cols: 80, rows: 24, pixelWidth: 0, pixelHeight: 0 },
      title: 'Page faster',
      agent: 'claude'
    })
    expect(await turns(scroll!.id)).toMatchObject([{ status: 'running', endedMs: null }])
    await b.setAgentStatus('devbox', pane.id, 'needsInput')
    expect(await turns(scroll!.id)).toMatchObject([
      { pane: pane.id, agent: 'claude', status: 'needsInput' }
    ])
    // A shell pane is not an agent: nothing recorded.
    const shell = (await b.listPanes('devbox')).find((p) => !p.agent && p.worktree === scroll!.id)!
    await b.setAgentStatus('devbox', shell.id, 'running')
    expect(await turns(scroll!.id)).toHaveLength(1)
  })

  it('is unsupported on an old civetd', async () => {
    const b = new MockBackend()
    const p = b.connectHost({ kind: 'ssh', destination: 'pre-git' })
    await vi.runAllTimersAsync()
    await p
    await expect(b.agentActivity('pre-git', '/w')).rejects.toMatchObject({ code: 'unsupported' })
  })
})

describe('MockBackend git actions', () => {
  /** Run a git action through the mock's fake delay. */
  async function act<T>(p: Promise<T>): Promise<T> {
    const settled = p.then(
      (v) => ({ ok: true as const, v }),
      (e: unknown) => ({ ok: false as const, e })
    )
    await vi.runAllTimersAsync()
    const r = await settled
    if (!r.ok) throw r.e
    return r.v
  }

  it('stages, discards with undo, commits, pushes and stashes', async () => {
    const { b } = await connected()
    const wt = (await b.listWorktrees('devbox', 'civet'))[1]!
    const paths = (s: { path: string }[]) => s.map((c) => c.path)
    const before = await b.gitStatus('devbox', wt.id)
    const untracked = before.untracked[0]!.path

    await act(b.gitAction('devbox', wt.id, { stage: { paths: [untracked] } }))
    let s = await b.gitStatus('devbox', wt.id)
    expect(paths(s.untracked)).not.toContain(untracked)
    expect(s.staged.find((c) => c.path === untracked)?.kind).toBe('added')
    await act(b.gitAction('devbox', wt.id, { unstage: { paths: [untracked] } }))
    expect(paths((await b.gitStatus('devbox', wt.id)).untracked)).toContain(untracked)

    const legacy = 'crates/civet-transport/src/legacy.rs'
    const snap = await act(
      b.gitAction('devbox', wt.id, { discard: { paths: [legacy, untracked], staged: false } })
    )
    expect(snap?.entries.map((e) => [e.path, e.untracked, e.file === null])).toEqual([
      [legacy, false, true],
      [untracked, true, false]
    ])
    s = await b.gitStatus('devbox', wt.id)
    expect([...paths(s.unstaged), ...paths(s.untracked)]).not.toContain(legacy)
    await act(b.gitAction('devbox', wt.id, { restoreDiscarded: snap! }))
    s = await b.gitStatus('devbox', wt.id)
    expect(paths(s.unstaged)).toContain(legacy)
    expect(paths(s.untracked)).toContain(untracked)

    // Nothing staged and not "all": git's refusal.
    await act(b.gitAction('devbox', wt.id, { unstage: { paths: paths(s.staged) } }))
    await expect(
      act(b.gitAction('devbox', wt.id, { commit: { message: 'x', amend: false, all: false } }))
    ).rejects.toMatchObject({ code: 'git' })
    const ahead = (await b.gitBranch('devbox', wt.id)).ahead
    await act(b.gitAction('devbox', wt.id, { commit: { message: 'x', amend: false, all: true } }))
    let br = await b.gitBranch('devbox', wt.id)
    expect(br.ahead).toBe(ahead + 1)
    expect(br.canUndoCommit).toBe(true)
    expect(changed(await b.gitStatus('devbox', wt.id))).toBe(0)
    await act(b.gitAction('devbox', wt.id, { undoCommit: { head: br.head! } }))
    expect((await b.gitStatus('devbox', wt.id)).staged.length).toBeGreaterThan(0)

    await act(b.gitAction('devbox', wt.id, 'stashAll'))
    expect(changed(await b.gitStatus('devbox', wt.id))).toBe(0)
    expect((await b.gitBranch('devbox', wt.id)).stashes).toBe(1)
    await expect(act(b.gitAction('devbox', wt.id, 'stashAll'))).rejects.toMatchObject({
      code: 'git'
    })
    await act(b.gitAction('devbox', wt.id, 'stashPop'))
    expect(changed(await b.gitStatus('devbox', wt.id))).toBeGreaterThan(0)

    // Behind after a fetch: push is rejected until a pull.
    await act(b.gitAction('devbox', wt.id, 'fetch'))
    expect((await b.gitBranch('devbox', wt.id)).behind).toBe(1)
    await expect(act(b.gitAction('devbox', wt.id, 'push'))).rejects.toMatchObject({
      message: expect.stringContaining('[rejected]')
    })
    await act(b.gitAction('devbox', wt.id, 'pull'))
    await act(b.gitAction('devbox', wt.id, 'push'))
    br = await b.gitBranch('devbox', wt.id)
    expect([br.ahead, br.behind, br.canUndoCommit]).toEqual([0, 0, false])
  })

  it('resolves conflicts and publishes a branch without an upstream', async () => {
    const { b } = await connected()
    const wts = await b.listWorktrees('devbox', 'civet')
    const flaky = wts.find((w) => w.path.endsWith('fix-flaky-e2e'))!
    const conflict = (await b.gitStatus('devbox', flaky.id)).conflicted[0]!.path
    await act(
      b.gitAction('devbox', flaky.id, { acceptConflict: { paths: [conflict], side: 'theirs' } })
    )
    const s = await b.gitStatus('devbox', flaky.id)
    expect(s.conflicted).toEqual([])
    expect(s.staged.map((c) => c.path)).toEqual([conflict])

    const fresh = wts.find((w) => !w.isMain && !/quic-resume|fix-flaky/.test(w.path))!
    expect((await b.gitBranch('devbox', fresh.id)).upstream).toBeNull()
    await expect(act(b.gitAction('devbox', fresh.id, 'pull'))).rejects.toMatchObject({
      message: expect.stringContaining('no upstream')
    })
    await act(b.gitAction('devbox', fresh.id, 'push'))
    expect((await b.gitBranch('devbox', fresh.id)).upstream).toBe(`origin/${fresh.branch}`)
  })

  it('a pre-0.1.14 civetd reports changes but refuses git actions', async () => {
    const b = new MockBackend()
    const p = b.connectHost({ kind: 'ssh', destination: 'pre-git-box' })
    await vi.runAllTimersAsync()
    await p
    const wt = (await b.listWorktrees('pre-git-box', 'civet'))[1]!
    expect((await b.gitStatus('pre-git-box', wt.id)).unstaged.length).toBeGreaterThan(0)
    await expect(b.gitBranch('pre-git-box', wt.id)).rejects.toMatchObject({ code: 'unsupported' })
    await expect(b.gitAction('pre-git-box', wt.id, 'fetch')).rejects.toMatchObject({
      code: 'unsupported'
    })
  })
})

function changed(s: { staged: unknown[]; unstaged: unknown[]; untracked: unknown[] }): number {
  return s.staged.length + s.unstaged.length + s.untracked.length
}
