// Agent activity timelines of worktrees (right panel → Activity).
//
// civetd records each agent turn (status, times, files changed) per worktree
// and finishes a turn's file list a moment after the status event, so the
// tab polls `agent_activity` while it shows instead of following events;
// never while the window is hidden or the host is not connected. A daemon
// too old for it (`unsupported`) is asked again only after a reconnect.

import { useEffect } from 'react'
import { create } from 'zustand'
import { CivetError } from '@/backend'
import type { AgentActivity } from '@/types'
import { useStore, wsKey } from './store'

/** Poll period while the Activity tab shows. */
export const ACTIVITY_POLL_MS = 3000

export type ActivityEntry = {
  activity: AgentActivity | null
  /** Last failure (cleared by the next success). */
  error: CivetError | null
}

export const useActivity = create<{ byWorktree: Record<string, ActivityEntry> }>(() => ({
  byWorktree: {}
}))

const inFlight = new Set<string>()

/** Fetch a worktree's timeline once (a no-op while a fetch for it runs). */
export async function refreshActivity(hostId: string, worktree: string): Promise<void> {
  const b = useStore.getState().backend
  const key = wsKey(hostId, worktree)
  if (!b || inFlight.has(key)) return
  inFlight.add(key)
  const set = (entry: ActivityEntry) =>
    useActivity.setState((s) => ({ byWorktree: { ...s.byWorktree, [key]: entry } }))
  try {
    const activity = await b.agentActivity(hostId, worktree)
    const prev = useActivity.getState().byWorktree[key]
    // Why: polls mostly return the same thing; keep the object so nothing re-renders.
    if (prev && !prev.error && JSON.stringify(prev.activity) === JSON.stringify(activity)) return
    set({ activity, error: null })
  } catch (err) {
    const prev = useActivity.getState().byWorktree[key]
    set({ activity: prev?.activity ?? null, error: CivetError.from(err) })
  } finally {
    inFlight.delete(key)
  }
}

/**
 * Poll a worktree's timeline every `intervalMs` while mounted, the host is
 * connected and the window visible (and right away when it becomes so).
 */
export function useActivityPolling(hostId: string, worktree: string, intervalMs: number): void {
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  useEffect(() => {
    if (!connected) return
    const key = wsKey(hostId, worktree)
    const tick = () => {
      if (document.visibilityState === 'hidden') return
      if (useActivity.getState().byWorktree[key]?.error?.code === 'unsupported') return
      void refreshActivity(hostId, worktree)
    }
    // A reconnect may have upgraded the daemon: ask again.
    const entry = useActivity.getState().byWorktree[key]
    if (entry?.error?.code === 'unsupported') {
      useActivity.setState((s) => ({
        byWorktree: { ...s.byWorktree, [key]: { ...entry, error: null } }
      }))
    }
    tick()
    const timer = setInterval(tick, intervalMs)
    document.addEventListener('visibilitychange', tick)
    return () => {
      clearInterval(timer)
      document.removeEventListener('visibilitychange', tick)
    }
  }, [hostId, worktree, intervalMs, connected])
}
