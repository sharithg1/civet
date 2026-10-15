import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import type { CivetBackend } from '../backend'
import type { HostSummary, PaneInfo, RepoInfo, WorktreeInfo } from '../types'
import { termSize } from '../types'
import { setPrefs } from './prefs'
import type { Tab } from './store'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, { window: { ...target, innerWidth: 1400 }, document: { ...target, hasFocus: () => true } })
})

const store = await import('./store')
const { useStore } = store

function summary(hostId: string, patch: Partial<HostSummary> = {}): HostSummary {
  return {
    hostId,
    label: hostId,
    state: 'connected',
    transport: 'quic',
    daemonVersion: '0.1.2',
    instanceId: '1',
    upgrade: null,
    ...patch
  }
}

function pane(id: number, patch: Partial<PaneInfo> = {}): PaneInfo {
  return {
    id,
    worktree: '/w',
    cwd: '/w',
    command: ['claude'],
    title: 't',
    size: termSize(80, 24),
    agent: 'claude',
    status: 'running',
    exitCode: null,
    exited: false,
    createdAtMs: 0,
    state: 1,
    ...patch
  }
}

function fakeBackend(over: Partial<CivetBackend> = {}): CivetBackend {
  const noop = async () => {}
  const un = async () => () => {}
  return {
    kind: 'mock',
    listHosts: async () => [],
    listRepos: async () => [],
    listPanes: async () => [],
    listWorktrees: async () => [],
    listPullRequests: async () => [],
    setPrediction: vi.fn(noop),
    notifyNetworkChange: noop,
    onHostState: un,
    onBootstrapProgress: un,
    onDaemonEvent: un,
    onAgentAttention: un,
    onClipboard: un,
    ...over
  } as unknown as CivetBackend
}

afterEach(() => {
  useStore.setState({ hosts: {}, hostOrder: [], panes: {}, toasts: [], confirm: null })
})

describe('store', () => {
  it('a reply snapshot never overwrites a newer status from events (L11)', () => {
    store.onDaemonEvent('h', { paneCreated: pane(5) })
    store.onDaemonEvent('h', {
      agentStatus: { pane: 5, status: 'needsInput', previous: 'running', source: 'hook', message: null, atMs: 1 }
    })
    // The launch_agent reply (taken when the pane was created) arrives late.
    store.addPaneSnapshot('h', pane(5, { status: 'running' }))
    expect(useStore.getState().panes.h![5]!.status).toBe('needsInput')
    store.addPaneSnapshot('h', pane(6))
    expect(useStore.getState().panes.h![6]).toBeDefined()
  })

  it('shows the daemon upgrade once and offers a confirmed restart', async () => {
    const restartDaemon = vi.fn(async (hostId: string) => summary(hostId, { instanceId: '2' }))
    useStore.setState({ backend: fakeBackend({ restartDaemon }) })
    const upgrade = { runningVersion: '0.1.1', availableVersion: '0.1.2', livePanes: 3 }
    for (let i = 0; i < 2; i++) {
      store.onHostState({ hostId: 'dev-arm', state: 'connected', transport: 'quic', rttMs: 40, message: null, upgrade })
    }
    const h = useStore.getState().hosts['dev-arm']!
    expect(h.upgrade).toEqual(upgrade)
    expect(h.daemonVersion).toBe('0.1.1')
    expect(store.upgradeSummary('dev-arm')).toBe('civetd v0.1.1 running · v0.1.2 available · 3 live panes')
    const toasts = useStore.getState().toasts.filter((t) => t.title.includes('0.1.2 is available'))
    expect(toasts).toHaveLength(1)

    store.requestRestartDaemon('dev-arm')
    const confirm = useStore.getState().confirm!
    expect(confirm.confirmLabel).toBe('Restart daemon (closes 3 panes)')
    expect(confirm.focusCancel).toBe(true)
    expect(restartDaemon).not.toHaveBeenCalled()
    confirm.onConfirm()
    await vi.waitFor(() => expect(restartDaemon).toHaveBeenCalledWith('dev-arm'))
    await vi.waitFor(() => expect(useStore.getState().hosts['dev-arm']!.upgrade).toBeNull())
  })

  it('reconnect goes through reconnect_host (Rust keeps the ssh options)', async () => {
    const reconnectHost = vi.fn(async (hostId: string) => summary(hostId))
    const connectHost = vi.fn()
    useStore.setState({ backend: fakeBackend({ reconnectHost, connectHost }) })
    store.onHostState({ hostId: 'box', state: 'failed', transport: null, rttMs: null, message: 'x', upgrade: null })
    await store.reconnectHost('box')
    expect(reconnectHost).toHaveBeenCalledWith('box')
    expect(connectHost).not.toHaveBeenCalled()
    expect(useStore.getState().hosts.box!.state).toBe('connected')
  })

  it('a busy incompatible daemon is remembered so the banner can offer a restart', async () => {
    const reconnectHost = vi.fn(async () => {
      throw { code: 'daemonBusy', message: 'incompatible and has live panes' }
    })
    useStore.setState({ backend: fakeBackend({ reconnectHost }) })
    store.onHostState({ hostId: 'box', state: 'failed', transport: null, rttMs: null, message: null, upgrade: null })
    await store.reconnectHost('box')
    const h = useStore.getState().hosts.box!
    expect(h.state).toBe('failed')
    expect(h.errorCode).toBe('daemonBusy')
  })

  it('pushes the local echo prediction preference to the core', async () => {
    const setPrediction = vi.fn(async () => {})
    const stop = await store.initApp(fakeBackend({ setPrediction }))
    expect(setPrediction).toHaveBeenLastCalledWith(true)
    setPrefs({ prediction: 'off' })
    expect(setPrediction).toHaveBeenLastCalledWith(false)
    setPrefs({ recentCount: 8 })
    expect(setPrediction).toHaveBeenCalledTimes(2)
    setPrefs({ prediction: 'auto' })
    expect(setPrediction).toHaveBeenLastCalledWith(true)
    stop()
    setPrefs({ prediction: 'off' })
    expect(setPrediction).toHaveBeenCalledTimes(3)
    setPrefs({ prediction: 'auto' })
  })

  describe('Codex tabs and steps', () => {
    const main: WorktreeInfo = { id: '/m', repo: 'r', path: '/m', branch: 'main', head: null, isMain: true, locked: false }
    function setup() {
      let next = 10
      const spawnPane = vi.fn(async (_h: string, req: { command: { program: string } | null; agent: string | null }) =>
        pane(next++, { worktree: '/m', command: req.command ? [req.command.program] : ['zsh'], agent: req.agent })
      )
      useStore.setState({
        backend: fakeBackend({ spawnPane, writeInput: vi.fn(async () => {}) } as Partial<CivetBackend>),
        repos: { h: [{ id: 'r', name: 'r', path: '/m', defaultBranch: 'main' }] },
        worktrees: { [store.wtKey('h', 'r')]: [main] },
        workspaces: {},
        removing: {}
      })
      return spawnPane
    }
    afterEach(() => setPrefs({ codexCommand: 'codex' }))

    it('⌥X opens the configured Codex command with the codex agent hint', async () => {
      const spawnPane = setup()
      setPrefs({ codexCommand: 'npx @openai/codex --full-auto' })
      await store.newAgentTab('h', '/m', 'codex')
      expect(spawnPane).toHaveBeenCalledWith(
        'h',
        expect.objectContaining({
          command: { program: 'npx', args: ['@openai/codex', '--full-auto'] },
          agent: 'codex'
        })
      )
    })

    it('a Codex on-create step runs codex and gets the first task', async () => {
      const spawnPane = setup()
      await store.createWorktreeWithSteps({
        hostId: 'h',
        repo: 'r',
        branch: '',
        base: null,
        steps: [
          { kind: 'codex', cmd: '' },
          { kind: 'agent', cmd: '' }
        ],
        prompt: 'fix the bug'
      })
      expect(spawnPane.mock.calls.map(([, req]) => req)).toEqual([
        expect.objectContaining({ command: { program: 'codex', args: ['fix the bug'] }, agent: 'codex' }),
        expect.objectContaining({ command: { program: 'claude', args: [] }, agent: 'claude' })
      ])
    })
  })

  describe('removeWorktree', () => {
    const repo: RepoInfo = { id: 'r', name: 'r', path: '/r', defaultBranch: 'main' }
    const wt = (id: string, patch: Partial<WorktreeInfo> = {}): WorktreeInfo => ({
      id,
      repo: 'r',
      path: id,
      branch: id.slice(1),
      head: null,
      isMain: false,
      locked: false,
      ...patch
    })
    const key = store.wsKey('h', '/feat')
    /** A backend whose remove_worktree waits until the test settles it. */
    function pendingRemove() {
      let settle!: { resolve: () => void; reject: (e: unknown) => void }
      // What git lists: /feat is gone once the removal succeeded.
      let onDisk = [wt('/main', { isMain: true }), wt('/feat'), wt('/other')]
      const removeWorktree = vi.fn(
        () =>
          new Promise<void>((resolve, reject) => {
            settle = {
              resolve: () => {
                onDisk = onDisk.filter((w) => w.id !== '/feat')
                resolve()
              },
              reject
            }
          })
      )
      const killPane = vi.fn(async () => {})
      useStore.setState({
        backend: fakeBackend({ removeWorktree, killPane, listWorktrees: async () => onDisk }),
        repos: { h: [repo] },
        worktrees: { [store.wtKey('h', 'r')]: [wt('/main', { isMain: true }), wt('/feat'), wt('/other')] },
        workspaces: {},
        removing: {},
        removeRefused: {},
        mru: []
      })
      store.selectWorktree('h', '/feat')
      /** /feat disappears from git without this removal (e.g. a terminal). */
      const vanish = () => (onDisk = onDisk.filter((w) => w.id !== '/feat'))
      return { removeWorktree, killPane, settle: () => settle, vanish }
    }
    const worktreeIds = () => useStore.getState().worktrees[store.wtKey('h', 'r')]!.map((w) => w.id)

    it('marks the worktree removing until it is gone, then selects the main one', async () => {
      const b = pendingRemove()
      const done = store.removeWorktree('h', '/feat', false)
      expect(useStore.getState().removing[key]).toBe(true)
      expect(store.isRemoving('h', '/feat')).toBe(true)
      b.settle().resolve()
      await done
      expect(useStore.getState().removing[key]).toBeUndefined()
      expect(worktreeIds()).toEqual(['/main', '/other'])
      expect(useStore.getState().active).toEqual({ hostId: 'h', worktree: '/main' })
    })

    it('a failure clears removing, keeps the worktree and reports the refusal', async () => {
      const b = pendingRemove()
      const done = store.removeWorktree('h', '/feat', false)
      b.settle().reject({ code: 'git', message: 'contains modified files' })
      await done
      const s = useStore.getState()
      expect(s.removing[key]).toBeUndefined()
      expect(s.removeRefused[key]).toBe('contains modified files')
      expect(worktreeIds()).toEqual(['/main', '/feat', '/other'])
      expect(s.active).toEqual({ hostId: 'h', worktree: '/feat' })
      expect(s.toasts.some((t) => t.kind === 'error' && t.title === 'Could not remove feat')).toBe(true)
    })

    it('ignores a second removal of the same worktree while one is in flight', async () => {
      const b = pendingRemove()
      const first = store.removeWorktree('h', '/feat', false)
      await store.removeWorktree('h', '/feat', true)
      expect(b.removeWorktree).toHaveBeenCalledTimes(1)
      expect(useStore.getState().removing[key]).toBe(true)
      b.settle().resolve()
      await first
      expect(useStore.getState().removing[key]).toBeUndefined()
    })

    it('leaves the selection alone when the user switched away meanwhile', async () => {
      const b = pendingRemove()
      const done = store.removeWorktree('h', '/feat', false)
      store.selectWorktree('h', '/other')
      expect(useStore.getState().active).toEqual({ hostId: 'h', worktree: '/other' })
      b.settle().resolve()
      await done
      expect(useStore.getState().active).toEqual({ hostId: 'h', worktree: '/other' })
      expect(worktreeIds()).toEqual(['/main', '/other'])
    })

    it('opens no new tabs or panes in a worktree being removed', async () => {
      const b = pendingRemove()
      const spawnPane = vi.fn()
      useStore.setState({ backend: fakeBackend({ removeWorktree: b.removeWorktree, spawnPane }) })
      const done = store.removeWorktree('h', '/feat', false)
      await store.newTerminalTab('h', '/feat')
      await store.newAgentTab('h', '/feat')
      await store.splitFocused('row')
      expect(spawnPane).not.toHaveBeenCalled()
      b.settle().resolve()
      await done
    })

    it('a worktree the host no longer knows counts as removed', async () => {
      const b = pendingRemove()
      const done = store.removeWorktree('h', '/feat', false)
      b.vanish()
      b.settle().reject({ code: 'notFound', message: 'worktree /feat not found' })
      await done
      const s = useStore.getState()
      expect(s.removeRefused[key]).toBeUndefined()
      expect(worktreeIds()).toEqual(['/main', '/other'])
      expect(s.active).toEqual({ hostId: 'h', worktree: '/main' })
      expect(s.toasts.some((t) => t.kind === 'error')).toBe(false)
    })

    it('a forced removal closes the sessions before git runs', async () => {
      const b = pendingRemove()
      useStore.setState({ panes: { h: { 7: pane(7, { worktree: '/feat' }), 8: pane(8, { worktree: '/other' }) } } })
      const done = store.removeWorktree('h', '/feat', true)
      expect(b.killPane).toHaveBeenCalledWith('h', 7, 'hangup')
      expect(b.killPane).toHaveBeenCalledTimes(1)
      b.settle().resolve()
      await done
    })
  })

  describe('worktree list sync', () => {
    const repo: RepoInfo = { id: 'r', name: 'r', path: '/r', defaultBranch: 'main' }
    const wt = (id: string, patch: Partial<WorktreeInfo> = {}): WorktreeInfo => ({
      id,
      repo: 'r',
      path: id,
      branch: id.slice(1),
      head: null,
      isMain: false,
      locked: false,
      ...patch
    })
    const main = wt('/main', { isMain: true })
    const ids = () => useStore.getState().worktrees[store.wtKey('h', 'r')]!.map((w) => w.id)
    /** list_worktrees calls the test answers one by one. */
    function pendingLists(initial: WorktreeInfo[]) {
      const calls: ((list: WorktreeInfo[]) => void)[] = []
      const listWorktrees = vi.fn(() => new Promise<WorktreeInfo[]>((resolve) => calls.push(resolve)))
      useStore.setState({
        backend: fakeBackend({ listWorktrees }),
        repos: { h: [repo] },
        worktrees: { [store.wtKey('h', 'r')]: initial },
        workspaces: {},
        removing: {},
        mru: [],
        active: null
      })
      return { listWorktrees, calls }
    }

    it('collapses refreshes that arrive while one is running into one trailing list', async () => {
      const b = pendingLists([main])
      const first = store.refreshWorktrees('h', 'r')
      void store.refreshWorktrees('h', 'r')
      void store.refreshWorktrees('h', 'r')
      expect(b.listWorktrees).toHaveBeenCalledTimes(1)
      b.calls[0]!([main, wt('/a')])
      await vi.waitFor(() => expect(b.listWorktrees).toHaveBeenCalledTimes(2))
      b.calls[1]!([main, wt('/a'), wt('/b')])
      await first
      expect(b.listWorktrees).toHaveBeenCalledTimes(2)
      expect(ids()).toEqual(['/main', '/a', '/b'])
    })

    it('drops a list taken before a create was applied and lists again', async () => {
      const b = pendingLists([main])
      const refresh = store.refreshWorktrees('h', 'r')
      // The create reply arrives while that (older) list is in flight.
      store.applyCreatedWorktree('h', wt('/new'))
      store.selectWorktree('h', '/new')
      expect(ids()).toEqual(['/main', '/new'])
      b.calls[0]!([main])
      await vi.waitFor(() => expect(b.listWorktrees).toHaveBeenCalledTimes(2))
      expect(ids()).toEqual(['/main', '/new'])
      b.calls[1]!([main, wt('/new')])
      await refresh
      expect(ids()).toEqual(['/main', '/new'])
      expect(useStore.getState().active).toEqual({ hostId: 'h', worktree: '/new' })
    })

    it('a worktree removed outside Civet loses its tabs and the selection', async () => {
      const b = pendingLists([main, wt('/feat')])
      const fkey = store.wsKey('h', '/feat')
      useStore.setState({ panes: { h: { 3: pane(3, { worktree: '/feat' }) } } })
      store.selectWorktree('h', '/feat')
      expect(useStore.getState().workspaces[fkey]!.tabs).toHaveLength(1)
      store.onDaemonEvent('h', { worktreesChanged: { repo: 'r' } })
      b.calls[0]!([main])
      await vi.waitFor(() => expect(ids()).toEqual(['/main']))
      const s = useStore.getState()
      expect(s.workspaces[fkey]).toBeUndefined()
      expect(s.mru).not.toContain(fkey)
      expect(s.active).toEqual({ hostId: 'h', worktree: '/main' })
      // The session keeps running on the host.
      expect(s.panes.h![3]).toBeDefined()
    })

    it('a failed list keeps the last known worktrees', async () => {
      useStore.setState({
        backend: fakeBackend({
          listWorktrees: async () => {
            throw { code: 'git', message: 'not a git repository' }
          }
        }),
        repos: { h: [repo] },
        worktrees: { [store.wtKey('h', 'r')]: [main, wt('/feat')] }
      })
      await store.refreshWorktrees('h', 'r')
      expect(ids()).toEqual(['/main', '/feat'])
    })
  })

  it('moveTab reorders tabs within a worktree and clamps the target', () => {
    const key = store.wsKey('h', '/w')
    const tab = (id: string, pane: number): Tab => ({ id, root: { type: 'leaf', pane }, focused: pane })
    useStore.setState({ workspaces: { [key]: { tabs: [tab('a', 1), tab('b', 2), tab('c', 3)], activeTab: 'a' } } })
    const order = () => useStore.getState().workspaces[key]!.tabs.map((t) => t.id)
    store.moveTab(key, 'a', 2)
    expect(order()).toEqual(['b', 'c', 'a'])
    store.moveTab(key, 'a', -5)
    expect(order()).toEqual(['a', 'b', 'c'])
    store.moveTab(key, 'missing', 0)
    expect(order()).toEqual(['a', 'b', 'c'])
    expect(useStore.getState().workspaces[key]!.activeTab).toBe('a')
  })
})
