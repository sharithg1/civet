// Git status of worktrees (right panel → Changes).
//
// civetd has no push events for working-tree edits, so the panel polls
// `git_status` while it is open (often while the Changes tab shows, slowly
// otherwise, for the tab's count) and so does an open diff tab; never while
// the window is hidden or the host is not connected. Pollers of the same
// worktree share fetches. Diffs are fetched on demand by the diff tab. The
// branch summary (`git_branch`: upstream, ahead / behind, stashes) is
// fetched with each status; a daemon too old for it leaves the git actions
// (state/git-actions.ts) greyed out.

import { useEffect } from 'react'
import { create } from 'zustand'
import { CivetError } from '@/backend'
import type { CivetBackend } from '@/backend'
import type { GitBranchInfo, GitFileChange, GitStatusInfo } from '@/types'
import { useStore, wsKey } from './store'

/** Poll period while the Changes tab is showing. */
export const CHANGES_POLL_MS = 2500
/** Poll period while the panel shows another tab (keeps the count fresh). */
export const CHANGES_BACKGROUND_POLL_MS = 15_000

/** Whether the daemon takes git actions: unknown until a `git_branch` answers. */
export type GitActionSupport = 'unknown' | 'supported' | 'unsupported'

export type ChangesEntry = {
  status: GitStatusInfo | null
  /** Last failure (cleared by the next success). */
  error: CivetError | null
  /** Last branch summary (kept across failed fetches). */
  branch?: GitBranchInfo | null
  actions?: GitActionSupport
}

type ChangesState = {
  /** wsKey(hostId, worktree) → last status */
  byWorktree: Record<string, ChangesEntry>
  /** wsKey → label of the git action running there ("Pushing…"). */
  busy: Record<string, string>
  /** wsKey → bumped after every git action, so open diff tabs fetch again. */
  diffEpoch: Record<string, number>
  /** wsKey → unsent commit message. */
  drafts: Record<string, string>
}

export const useChanges = create<ChangesState>(() => ({
  byWorktree: {},
  busy: {},
  diffEpoch: {},
  drafts: {}
}))

const inFlight = new Set<string>()
/** Keys whose forced refresh arrived while a fetch was running: fetch again after it. */
const queued = new Set<string>()
/** wsKey → when its last fetch started (ms). */
const fetchedAt = new Map<string, number>()

function setEntry(key: string, entry: ChangesEntry): void {
  useChanges.setState((s) => ({ byWorktree: { ...s.byWorktree, [key]: entry } }))
}

/**
 * Fetch a worktree's status (and branch summary) once: a no-op while a fetch
 * for it is running, or if one started less than `minAgeMs` ago (pollers pass
 * their period's half). `force` (after a git action): if a fetch is running,
 * fetch again once it is done, since it may predate the action.
 */
export async function refreshChanges(
  hostId: string,
  worktree: string,
  minAgeMs = 0,
  opts: { force?: boolean } = {}
): Promise<void> {
  const b = useStore.getState().backend
  const key = wsKey(hostId, worktree)
  if (!b) return
  if (inFlight.has(key)) {
    if (opts.force) queued.add(key)
    return
  }
  if (Date.now() - (fetchedAt.get(key) ?? -Infinity) < minAgeMs) return
  fetchedAt.set(key, Date.now())
  inFlight.add(key)
  try {
    const prev = useChanges.getState().byWorktree[key]
    const [status, branch] = await Promise.all([
      b.gitStatus(hostId, worktree),
      fetchBranch(b, hostId, worktree, prev)
    ])
    const next: ChangesEntry = { status, error: null, ...branch }
    const latest = useChanges.getState().byWorktree[key]
    // Why: polls mostly return the same thing; keep the object so nothing re-renders.
    if (latest && JSON.stringify(latest) === JSON.stringify(next)) return
    setEntry(key, next)
  } catch (err) {
    const prev = useChanges.getState().byWorktree[key]
    setEntry(key, { ...prev, status: prev?.status ?? null, error: CivetError.from(err) })
  } finally {
    inFlight.delete(key)
    if (queued.delete(key)) void refreshChanges(hostId, worktree, 0, { force: true })
  }
}

/** The branch summary; never rejects (a failure keeps the last one). */
async function fetchBranch(
  b: CivetBackend,
  hostId: string,
  worktree: string,
  prev: ChangesEntry | undefined
): Promise<Pick<ChangesEntry, 'branch' | 'actions'>> {
  if (prev?.actions === 'unsupported') return { branch: null, actions: 'unsupported' }
  try {
    return { branch: await b.gitBranch(hostId, worktree), actions: 'supported' }
  } catch (err) {
    if (CivetError.from(err).code === 'unsupported') return { branch: null, actions: 'unsupported' }
    return { branch: prev?.branch ?? null, actions: prev?.actions ?? 'unknown' }
  }
}

/**
 * Poll a worktree's status every `intervalMs` while mounted, the host is
 * connected and the window visible (and right away when it becomes so).
 * Stops after an `unsupported` error (an older civetd) until the host
 * connects again: restarting the daemon is how it gets upgraded.
 */
export function useChangesPolling(hostId: string, worktree: string, intervalMs: number): void {
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  useEffect(() => {
    if (!connected) return
    const key = wsKey(hostId, worktree)
    const entry = useChanges.getState().byWorktree[key]
    const retry = entry?.error?.code === 'unsupported' || entry?.actions === 'unsupported'
    if (retry) setEntry(key, { ...entry, error: null, actions: 'unknown' })
    const tick = (now = false) => {
      if (document.visibilityState === 'hidden') return
      if (useChanges.getState().byWorktree[key]?.error?.code === 'unsupported') return
      void refreshChanges(hostId, worktree, now ? 0 : intervalMs / 2)
    }
    // Why: after clearing a verdict, ask again now rather than show "unknown" until the next tick.
    tick(retry)
    const onTick = () => tick()
    const timer = setInterval(onTick, intervalMs)
    document.addEventListener('visibilitychange', onTick)
    return () => {
      clearInterval(timer)
      document.removeEventListener('visibilitychange', onTick)
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
