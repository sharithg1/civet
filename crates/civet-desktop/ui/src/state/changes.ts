// Git status of worktrees (right panel → Changes).
//
// civetd has no push events for working-tree edits, so the panel polls
// `git_status` while it is open (often while the Changes tab shows, slowly
// otherwise, for the tab's count) and so does an open diff tab; never while
// the window is hidden or the host is not connected. Pollers of the same
// worktree share fetches. Diffs are fetched on demand by the diff tab.

import { useEffect } from 'react'
import { create } from 'zustand'
import { CivetError } from '@/backend'
import type { GitFileChange, GitStatusInfo } from '@/types'
import { useStore, wsKey } from './store'

/** Poll period while the Changes tab is showing. */
export const CHANGES_POLL_MS = 2500
/** Poll period while the panel shows another tab (keeps the count fresh). */
export const CHANGES_BACKGROUND_POLL_MS = 15_000

export type ChangesEntry = {
  status: GitStatusInfo | null
  /** Last failure (cleared by the next success). */
  error: CivetError | null
}

type ChangesState = {
  /** wsKey(hostId, worktree) → last status */
  byWorktree: Record<string, ChangesEntry>
}

export const useChanges = create<ChangesState>(() => ({ byWorktree: {} }))

const inFlight = new Set<string>()
/** wsKey → when its last fetch started (ms). */
const fetchedAt = new Map<string, number>()

function setEntry(key: string, entry: ChangesEntry): void {
  useChanges.setState((s) => ({ byWorktree: { ...s.byWorktree, [key]: entry } }))
}

/**
 * Fetch a worktree's status once: a no-op while a fetch for it is running, or
 * if one started less than `minAgeMs` ago (pollers pass their period's half).
 */
export async function refreshChanges(
  hostId: string,
  worktree: string,
  minAgeMs = 0
): Promise<void> {
  const b = useStore.getState().backend
  const key = wsKey(hostId, worktree)
  if (!b || inFlight.has(key)) return
  if (Date.now() - (fetchedAt.get(key) ?? -Infinity) < minAgeMs) return
  fetchedAt.set(key, Date.now())
  inFlight.add(key)
  try {
    const status = await b.gitStatus(hostId, worktree)
    const prev = useChanges.getState().byWorktree[key]
    // Why: polls mostly return the same thing; keep the object so nothing re-renders.
    if (prev?.status && !prev.error && JSON.stringify(prev.status) === JSON.stringify(status))
      return
    setEntry(key, { status, error: null })
  } catch (err) {
    const prev = useChanges.getState().byWorktree[key]
    setEntry(key, { status: prev?.status ?? null, error: CivetError.from(err) })
  } finally {
    inFlight.delete(key)
  }
}

/**
 * Poll a worktree's status every `intervalMs` while mounted, the host is
 * connected and the window visible (and right away when it becomes so).
 * Stops after an `unsupported` error (an older civetd).
 */
export function useChangesPolling(hostId: string, worktree: string, intervalMs: number): void {
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  useEffect(() => {
    if (!connected) return
    const key = wsKey(hostId, worktree)
    const tick = () => {
      if (document.visibilityState === 'hidden') return
      if (useChanges.getState().byWorktree[key]?.error?.code === 'unsupported') return
      void refreshChanges(hostId, worktree, intervalMs / 2)
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

/** Changed paths (a file both staged and modified again counts once). */
export function changedFileCount(status: GitStatusInfo | null | undefined): number {
  if (!status) return 0
  const paths = new Set<string>()
  for (const list of [status.conflicted, status.staged, status.unstaged, status.untracked]) {
    for (const c of list) paths.add(c.path)
  }
  return paths.size
}

/** Which side of a change a diff is asked for. */
export type ChangeSide = 'staged' | 'unstaged' | 'untracked' | 'conflicted'

/** A file picked in the list: enough to ask for its diff and find it again. */
export type ChangeRef = { side: ChangeSide; path: string; oldPath: string | null }

export function findChange(
  status: GitStatusInfo | null | undefined,
  ref: ChangeRef
): GitFileChange | undefined {
  return status?.[ref.side].find((c) => c.path === ref.path)
}
