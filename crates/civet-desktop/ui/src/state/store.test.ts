import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import type { CivetBackend } from '../backend'
import type { HostSummary, PaneInfo } from '../types'
import { termSize } from '../types'
import { setPrefs } from './prefs'

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
})
