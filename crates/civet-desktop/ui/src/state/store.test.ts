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
      const removeWorktree = vi.fn(
        () => new Promise<void>((resolve, reject) => (settle = { resolve, reject }))
      )
      const killPane = vi.fn(async () => {})
      useStore.setState({
        backend: fakeBackend({ removeWorktree, killPane }),
        repos: { h: [repo] },
        worktrees: { [store.wtKey('h', 'r')]: [wt('/main', { isMain: true }), wt('/feat'), wt('/other')] },
        workspaces: {},
        removing: {},
        removeRefused: {},
        mru: []
      })
      store.selectWorktree('h', '/feat')
      return { removeWorktree, killPane, settle: () => settle }
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
