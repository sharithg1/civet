import { describe, expect, it, vi } from 'vitest'
import { CivetError, CivetEvents, TauriBackend, type TauriDeps } from './backend'
import { encodeRecords, parseRecords } from './terminal/records'
import { termSize } from './types'

class FakeChannel<T> {
  static last: FakeChannel<unknown> | null = null
  id = 42
  onmessage: (m: T) => void = () => {}
  constructor() {
    FakeChannel.last = this as FakeChannel<unknown>
  }
}

function setup(impl?: (cmd: string, args?: Record<string, unknown>) => unknown) {
  const invoke = vi.fn(
    async (cmd: string, args?: Record<string, unknown>) => impl?.(cmd, args) ?? null
  )
  const listeners = new Map<string, (e: { payload: unknown }) => void>()
  const unlisten = vi.fn()
  const listen = vi.fn(async (event: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(event, cb)
    return unlisten
  })
  const deps = { invoke, listen, Channel: FakeChannel } as unknown as TauriDeps
  return { backend: new TauriBackend(deps), invoke, listen, listeners, unlisten }
}

describe('TauriBackend commands', () => {
  it('uses the documented command names and camelCase argument keys', async () => {
    const { backend, invoke } = setup()
    await backend.connectHost({ kind: 'ssh', destination: 'dev@box' }, 'ssh')
    await backend.disconnectHost('dev@box')
    await backend.listHosts()
    await backend.notifyNetworkChange()
    await backend.listRepos('h')
    await backend.addRepo('h', '/src/x')
    await backend.removeRepo('h', 'x')
    await backend.listWorktrees('h', 'x')
    await backend.listPullRequests('h', 'x')
    await backend.gitStatus('h', '/w')
    await backend.gitDiff('h', '/w', 'b.rs', { oldPath: 'a.rs', staged: true })
    await backend.gitBranch('h', '/w')
    await backend.gitAction('h', '/w', { stage: { paths: ['a.rs'] } })
    await backend.gitAction('h', '/w', 'push')
    await backend.createWorktree('h', 'x', 'feat')
    await backend.removeWorktree('h', '/w', true)
    await backend.listFiles('h', '/w', 'src', false)
    await backend.readFile('h', '/w', 'src/a.ts', 1024)
    await backend.listAllFiles('h', '/w')
    await backend.listPanes('h')
    await backend.writeInput('h', 3, 'ls\r')
    await backend.writeInputBinary('h', 3, '\x00\xff')
    await backend.resizePane('h', 3, termSize(120, 40))
    await backend.killPane('h', 3, 'terminate')
    await backend.killPane('h', 4)
    await backend.fetchScrollback('h', 3, { before: { end: 500, limit: 100 } })
    await backend.setAgentStatus('h', 3, 'done')
    await backend.setPrediction(true)
    await backend.ping('h')
    await backend.daemonInfo('h')
    await backend.reconnectHost('dev@box')
    await backend.restartDaemon('local')
    expect(invoke.mock.calls).toEqual([
      ['connect_host', { target: { kind: 'ssh', destination: 'dev@box' }, transport: 'ssh' }],
      ['disconnect_host', { hostId: 'dev@box' }],
      ['list_hosts', {}],
      ['notify_network_change', {}],
      ['list_repos', { hostId: 'h' }],
      ['add_repo', { hostId: 'h', path: '/src/x' }],
      ['remove_repo', { hostId: 'h', repo: 'x' }],
      ['list_worktrees', { hostId: 'h', repo: 'x' }],
      ['list_pull_requests', { hostId: 'h', repo: 'x' }],
      ['git_status', { hostId: 'h', worktree: '/w' }],
      [
        'git_diff',
        {
          hostId: 'h',
          worktree: '/w',
          path: 'b.rs',
          oldPath: 'a.rs',
          staged: true,
          contextLines: 3
        }
      ],
      ['git_branch', { hostId: 'h', worktree: '/w' }],
      ['git_action', { hostId: 'h', worktree: '/w', action: { stage: { paths: ['a.rs'] } } }],
      ['git_action', { hostId: 'h', worktree: '/w', action: 'push' }],
      ['create_worktree', { hostId: 'h', repo: 'x', branch: 'feat', base: null, path: null }],
      ['remove_worktree', { hostId: 'h', worktree: '/w', force: true }],
      ['list_files', { hostId: 'h', worktree: '/w', dir: 'src', includeIgnored: false }],
      ['read_file', { hostId: 'h', worktree: '/w', path: 'src/a.ts', maxBytes: 1024 }],
      ['list_all_files', { hostId: 'h', worktree: '/w' }],
      ['list_panes', { hostId: 'h' }],
      ['write_input', { hostId: 'h', pane: 3, data: 'ls\r' }],
      ['write_input_binary', { hostId: 'h', pane: 3, data: '\x00\xff' }],
      [
        'resize_pane',
        { hostId: 'h', pane: 3, size: { cols: 120, rows: 40, pixelWidth: 0, pixelHeight: 0 } }
      ],
      ['kill_pane', { hostId: 'h', pane: 3, signal: 'terminate' }],
      ['kill_pane', { hostId: 'h', pane: 4 }],
      ['fetch_scrollback', { hostId: 'h', pane: 3, query: { before: { end: 500, limit: 100 } } }],
      ['set_agent_status', { hostId: 'h', pane: 3, status: 'done' }],
      ['set_prediction', { enabled: true }],
      ['ping', { hostId: 'h' }],
      ['daemon_info', { hostId: 'h' }],
      ['reconnect_host', { hostId: 'dev@box' }],
      ['restart_daemon', { hostId: 'local' }]
    ])
  })

  it('defaults connect_host transport to auto', async () => {
    const { backend, invoke } = setup()
    await backend.connectHost({ kind: 'local' })
    expect(invoke).toHaveBeenCalledWith('connect_host', {
      target: { kind: 'local' },
      transport: 'auto'
    })
  })

  it('normalizes rejections into CivetError { code, message }', async () => {
    const { backend } = setup((cmd) => {
      if (cmd === 'list_repos') throw { code: 'noSuchHost', message: 'no such host: h' }
      if (cmd === 'list_panes') throw 'plain string'
      return null
    })
    await expect(backend.listRepos('h')).rejects.toMatchObject({
      name: 'CivetError',
      code: 'noSuchHost',
      message: 'no such host: h'
    })
    await expect(backend.listPanes('h')).rejects.toBeInstanceOf(CivetError)
    await expect(backend.listPanes('h')).rejects.toMatchObject({
      code: 'unknown',
      message: 'plain string'
    })
  })
})

describe('TauriBackend.attachPane', () => {
  it('passes a Channel as onOutput and delivers messages (even before invoke resolves)', async () => {
    const received: Uint8Array[] = []
    let resolveAttach!: (v: unknown) => void
    const { backend, invoke } = setup((cmd) =>
      cmd === 'attach_pane' ? new Promise((r) => (resolveAttach = r)) : null
    )
    const pending = backend.attachPane('h', 7, (m) => received.push(m))
    await Promise.resolve()
    const channel = FakeChannel.last!
    expect(invoke).toHaveBeenCalledWith('attach_pane', { hostId: 'h', pane: 7, onOutput: channel })
    // Full record arrives before the command resolves.
    const msg = encodeRecords([{ state: 1, cols: 80, rows: 24, payload: 'hello' }])
    channel.onmessage(msg.buffer)
    resolveAttach({ pane: 7, state: 1, size: termSize(80, 24), historyLines: 0, lastInputSeq: 0 })
    const att = await pending
    expect(att.info.pane).toBe(7)
    expect(received).toHaveLength(1)
    expect(new TextDecoder().decode(parseRecords(received[0]!)[0]!.payload)).toBe('hello')

    await att.detach()
    expect(invoke).toHaveBeenLastCalledWith('detach_pane', { hostId: 'h', pane: 7 })
    channel.onmessage(msg.buffer)
    expect(received).toHaveLength(1) // nothing after detach
    await att.detach() // idempotent
    expect(invoke.mock.calls.filter(([c]) => c === 'detach_pane')).toHaveLength(1)
  })

  it('drops malformed channel messages instead of throwing', async () => {
    const received: Uint8Array[] = []
    const { backend } = setup((cmd) =>
      cmd === 'attach_pane'
        ? { pane: 1, state: 0, size: termSize(80, 24), historyLines: 0, lastInputSeq: 0 }
        : null
    )
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})
    await backend.attachPane('h', 1, (m) => received.push(m))
    FakeChannel.last!.onmessage('not bytes')
    FakeChannel.last!.onmessage([1, 2, 3])
    expect(received).toHaveLength(1)
    expect(spy).toHaveBeenCalled()
    spy.mockRestore()
  })

  it('rejects with CivetError when attach fails', async () => {
    const { backend } = setup((cmd) => {
      if (cmd === 'attach_pane') throw { code: 'notFound', message: 'pane 9 not found' }
      return null
    })
    await expect(backend.attachPane('h', 9, () => {})).rejects.toMatchObject({ code: 'notFound' })
  })
})

describe('TauriBackend events', () => {
  it('subscribes to the documented event names and unwraps payloads', async () => {
    const { backend, listeners, unlisten } = setup()
    const hostState = vi.fn()
    const progress = vi.fn()
    const daemon = vi.fn()
    const attention = vi.fn()
    const un = await backend.onHostState(hostState)
    await backend.onBootstrapProgress(progress)
    await backend.onDaemonEvent(daemon)
    await backend.onAgentAttention(attention)
    expect([...listeners.keys()]).toEqual([
      'civet://host-state',
      'civet://bootstrap-progress',
      'civet://daemon-event',
      'civet://agent-attention'
    ])
    const hs = { hostId: 'h', state: 'reconnecting', transport: 'quic', rttMs: null, message: null }
    listeners.get(CivetEvents.hostState)!({ payload: hs })
    expect(hostState).toHaveBeenCalledWith(hs)
    const de = { hostId: 'h', event: { paneTitle: { pane: 3, title: 'vim' } } }
    listeners.get(CivetEvents.daemonEvent)!({ payload: de })
    expect(daemon).toHaveBeenCalledWith(de)
    un()
    expect(unlisten).toHaveBeenCalled()
  })
})

describe('TauriBackend attach/detach ordering', () => {
  it('waits for an in-flight detach of the same pane before re-attaching', async () => {
    const order: string[] = []
    let releaseDetach!: () => void
    const { backend } = setup((cmd) => {
      order.push(cmd)
      if (cmd === 'detach_pane') return new Promise<null>((r) => (releaseDetach = () => r(null)))
      if (cmd === 'attach_pane') {
        return { pane: 1, state: 0, size: termSize(80, 24), historyLines: 0, lastInputSeq: 0 }
      }
      return null
    })
    const a = await backend.attachPane('h', 1, () => {})
    const detached = a.detach()
    const again = backend.attachPane('h', 1, () => {})
    await Promise.resolve()
    await Promise.resolve()
    expect(order).toEqual(['attach_pane', 'detach_pane'])
    releaseDetach()
    await detached
    await again
    expect(order).toEqual(['attach_pane', 'detach_pane', 'attach_pane'])
  })

  it("a stale attachment's detach doesn't detach its successor", async () => {
    const order: string[] = []
    let releaseFirst!: () => void
    let attaches = 0
    const info = { pane: 1, state: 0, size: termSize(80, 24), historyLines: 0, lastInputSeq: 0 }
    const { backend } = setup((cmd) => {
      order.push(cmd)
      if (cmd === 'attach_pane' && attaches++ === 0) {
        return new Promise((r) => (releaseFirst = () => r(info)))
      }
      return cmd === 'attach_pane' ? info : null
    })
    // Leave before the first attach resolves, then re-open.
    const first = backend.attachPane('h', 1, () => {})
    const second = backend.attachPane('h', 1, () => {})
    await Promise.resolve()
    expect(order).toEqual(['attach_pane']) // the second waits its turn
    releaseFirst()
    await (await first).detach()
    const b = await second
    expect(order).toEqual(['attach_pane', 'attach_pane'])
    await b.detach()
    expect(order).toEqual(['attach_pane', 'attach_pane', 'detach_pane'])
  })
})
