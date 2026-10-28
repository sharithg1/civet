// Agent usage (tokens and estimated spend) of every connected host whose
// daemon has it, for the stats view's usage panel and the sidebar's
// per-worktree spend.
//
// Polled (`agent_usage`, the last USAGE_DAYS days) every POLL_MS per host
// while the sidebar shows spend (`sidebarSpend`) or the stats view is open,
// and right away when a host connects or the stats view opens. The daemon
// keeps what it parsed of the agents' logs, so a poll only reads what they
// appended since.

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import type { AgentUsage } from '@/types'
import { versionAtLeast } from './fileops'
import { onPrefsChange, usePrefs } from './prefs'
import { useStore, type HostView } from './store'

/** Oldest civetd with `AgentUsage` (civet_proto::AGENT_USAGE_MIN_DAEMON_VERSION). */
export const AGENT_USAGE_MIN_DAEMON_VERSION = '0.4.4'

/** The window asked for. */
export const USAGE_DAYS = 30

const POLL_MS = 60_000
const DAY_MS = 86_400_000

export type AgentUsageView = {
  usage: AgentUsage | null
  /** Last poll failed (usage is then the last good one). */
  error: string | null
}

export const useAgentUsage = create<{ hosts: Record<string, AgentUsageView> }>(() => ({
  hosts: {}
}))

export function hasAgentUsage(h: HostView | undefined): boolean {
  return (
    h?.state === 'connected' &&
    !!h.daemonVersion &&
    versionAtLeast(h.daemonVersion, AGENT_USAGE_MIN_DAEMON_VERSION)
  )
}

let backend: CivetBackend | null = null
/** Hosts being polled → their timer. */
const timers = new Map<string, ReturnType<typeof setTimeout>>()
/** Hosts with a poll awaiting its answer. */
const inFlight = new Set<string>()

function setView(hostId: string, f: (v: AgentUsageView) => AgentUsageView): void {
  useAgentUsage.setState((s) => ({
    hosts: { ...s.hosts, [hostId]: f(s.hosts[hostId] ?? { usage: null, error: null }) }
  }))
}

async function poll(hostId: string): Promise<void> {
  if (!backend || inFlight.has(hostId) || !timers.has(hostId)) return
  inFlight.add(hostId)
  try {
    const usage = await backend.agentUsage(hostId, Date.now() - USAGE_DAYS * DAY_MS)
    if (timers.has(hostId)) setView(hostId, () => ({ usage, error: null }))
  } catch (err) {
    if (timers.has(hostId)) setView(hostId, (v) => ({ ...v, error: CivetError.from(err).message }))
  } finally {
    inFlight.delete(hostId)
  }
  if (timers.has(hostId)) schedule(hostId, POLL_MS)
}

function schedule(hostId: string, ms: number): void {
  clearTimeout(timers.get(hostId))
  timers.set(
    hostId,
    setTimeout(() => void poll(hostId), ms)
  )
}

function stop(hostId: string): void {
  clearTimeout(timers.get(hostId))
  timers.delete(hostId)
}

/** Something shows usage: the sidebar's spend, or the stats view. */
function wanted(): boolean {
  return usePrefs.getState().sidebarSpend || useStore.getState().statsOpen
}

/** Follow connected hosts while usage is shown; returns the cleanup. */
export function initUsage(be: CivetBackend): () => void {
  backend = be
  const sync = () => {
    const hosts = useStore.getState().hosts
    const on = wanted()
    for (const id of [...timers.keys()]) if (!on || !hasAgentUsage(hosts[id])) stop(id)
    if (!on) return
    for (const [id, h] of Object.entries(hosts))
      if (hasAgentUsage(h) && !timers.has(id)) schedule(id, 0)
  }
  sync()
  const unsub = useStore.subscribe((s, prev) => {
    if (s.hosts !== prev.hosts || s.statsOpen !== prev.statsOpen) sync()
    // The stats view opened: bring its host's usage up to date.
    const id = s.active?.hostId
    if (s.statsOpen && !prev.statsOpen && id && timers.has(id)) schedule(id, 0)
  })
  const unsubPrefs = onPrefsChange((next, prev) => {
    if (next.sidebarSpend !== prev.sidebarSpend) sync()
  })
  return () => {
    unsub()
    unsubPrefs()
    for (const id of [...timers.keys()]) stop(id)
  }
}
