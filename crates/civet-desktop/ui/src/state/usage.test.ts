import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import type { CivetBackend } from '../backend'
import { setPrefs } from './prefs'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore } = await import('./store')
const { initUsage } = await import('./usage')

const host = (daemonVersion: string) => ({
  hostId: 'devbox',
  label: 'devbox',
  state: 'connected' as const,
  transport: 'quic' as const,
  daemonVersion,
  instanceId: null,
  upgrade: null,
  rttMs: null,
  message: null,
  bootstrap: null,
  errorCode: null
})

function fakeBackend() {
  const agentUsage = vi.fn(async () => ({
    sinceMs: 0,
    scannedAtMs: 0,
    sessions: [],
    prices: [],
    pricesAsOf: '2026-10-27'
  }))
  return { be: { agentUsage } as unknown as CivetBackend, agentUsage }
}

let cleanup: (() => void) | null = null
afterEach(() => {
  cleanup?.()
  cleanup = null
  vi.useRealTimers()
})

describe('agent usage polling', () => {
  it('polls only while the sidebar or the stats view shows usage', async () => {
    vi.useFakeTimers()
    setPrefs({ sidebarSpend: false })
    useStore.setState({ hosts: { devbox: host('0.4.4') }, statsOpen: false })
    const { be, agentUsage } = fakeBackend()
    cleanup = initUsage(be)
    await vi.advanceTimersByTimeAsync(120_000)
    expect(agentUsage).not.toHaveBeenCalled()

    useStore.setState({ statsOpen: true })
    await vi.advanceTimersByTimeAsync(10)
    expect(agentUsage).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(60_000)
    expect(agentUsage).toHaveBeenCalledTimes(2)

    useStore.setState({ statsOpen: false })
    await vi.advanceTimersByTimeAsync(120_000)
    expect(agentUsage).toHaveBeenCalledTimes(2)

    setPrefs({ sidebarSpend: true })
    await vi.advanceTimersByTimeAsync(10)
    expect(agentUsage).toHaveBeenCalledTimes(3)
  })

  it('leaves daemons without agent usage alone', async () => {
    vi.useFakeTimers()
    setPrefs({ sidebarSpend: true })
    useStore.setState({ hosts: { devbox: host('0.4.3') }, statsOpen: false })
    const { be, agentUsage } = fakeBackend()
    cleanup = initUsage(be)
    await vi.advanceTimersByTimeAsync(120_000)
    expect(agentUsage).not.toHaveBeenCalled()
  })
})
