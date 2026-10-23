// Host stats of the active worktree's host (title bar indicator, StatsView).
//
// Polled from the daemon (`host_stats`) every sample interval while the
// host is connected, its daemon has stats and Settings shows them
// (`showHostStats`); with processes while the stats view is open. Each host keeps its history here when another one becomes
// active, and the daemon fills in what was missed on the way back.

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import { mergeStats, newestSeq } from '@/lib/stats'
import type { HostStats } from '@/types'
import { versionAtLeast } from './fileops'
import { onPrefsChange, usePrefs } from './prefs'
import { useStore, type HostView } from './store'

/** Oldest civetd with `HostStats` (civet_proto::HOST_STATS_MIN_DAEMON_VERSION). */
export const HOST_STATS_MIN_DAEMON_VERSION = '0.2.1'

/** Between polls when the daemon has not said its interval yet, or after a failure. */
const FALLBACK_INTERVAL_MS = 2000

export type HostStatsView = {
  stats: HostStats | null
  /** Last poll failed (stats are then the last good ones). */
  error: string | null
}

export const useHostStats = create<{ hosts: Record<string, HostStatsView> }>(() => ({
  hosts: {}
}))

export function hasHostStats(h: HostView | undefined): boolean {
  return (
    h?.state === 'connected' &&
    !!h.daemonVersion &&
    versionAtLeast(h.daemonVersion, HOST_STATS_MIN_DAEMON_VERSION)
  )
}

let backend: CivetBackend | null = null
/** The host being polled. */
let current: string | null = null
/** Bumped on every start / stop, so a stale poll's answer is dropped. */
let generation = 0
let timer: ReturnType<typeof setTimeout> | null = null
/** Generation of the poll awaiting its answer. */
let inFlight: number | null = null
/** The stats view opened during a poll without processes: poll again right after it. */
let processPollPending = false

function setView(hostId: string, f: (v: HostStatsView) => HostStatsView): void {
  useHostStats.setState((s) => ({
    hosts: { ...s.hosts, [hostId]: f(s.hosts[hostId] ?? { stats: null, error: null }) }
  }))
}

async function poll(hostId: string, gen: number): Promise<void> {
  if (!backend || inFlight === gen) return
  inFlight = gen
  let interval = FALLBACK_INTERVAL_MS
  try {
    const prev = useHostStats.getState().hosts[hostId]?.stats ?? null
    const next = await backend.hostStats(hostId, newestSeq(prev), useStore.getState().statsOpen)
    if (gen !== generation) return
    interval = next.intervalMs || FALLBACK_INTERVAL_MS
    setView(hostId, (v) => ({ stats: mergeStats(v.stats, next), error: null }))
  } catch (err) {
    if (gen !== generation) return
    setView(hostId, (v) => ({ ...v, error: CivetError.from(err).message }))
  } finally {
    if (inFlight === gen) inFlight = null
  }
  if (gen === generation) {
    const now = processPollPending && useStore.getState().statsOpen
    processPollPending = false
    if (timer) clearTimeout(timer)
    timer = setTimeout(() => void poll(hostId, gen), now ? 0 : interval)
  }
}

function stop(): void {
  generation++
  current = null
  inFlight = null
  processPollPending = false
  if (timer) clearTimeout(timer)
  timer = null
}

function start(hostId: string): void {
  stop()
  current = hostId
  void poll(hostId, generation)
}

/** Poll the active host now (the stats view just opened and wants processes). */
function pollNow(): void {
  if (!current) return
  if (inFlight === generation) {
    processPollPending = true
    return
  }
  if (timer) clearTimeout(timer)
  timer = null
  void poll(current, generation)
}

/** Follow the active host; returns the cleanup. */
export function initStats(be: CivetBackend): () => void {
  backend = be
  const sync = () => {
    const s = useStore.getState()
    const hostId = s.active?.hostId ?? null
    const on = usePrefs.getState().showHostStats
    const want = on && hostId && hasHostStats(s.hosts[hostId]) ? hostId : null
    if (want === current) return
    if (want) start(want)
    else stop()
  }
  sync()
  const unsub = useStore.subscribe((s, prev) => {
    if (s.active?.hostId !== prev.active?.hostId || s.hosts !== prev.hosts) sync()
    if (s.statsOpen && !prev.statsOpen) pollNow()
  })
  const unsubPrefs = onPrefsChange((next, prev) => {
    if (next.showHostStats === prev.showHostStats) return
    if (!next.showHostStats) useStore.setState({ statsOpen: false })
    sync()
  })
  return () => {
    unsub()
    unsubPrefs()
    stop()
  }
}
