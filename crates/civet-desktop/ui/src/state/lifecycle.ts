// Worktree lifecycle: each worktree's ahead / behind against the default
// branch (sidebar), "Clean up worktrees" (stale ones: merged, remote branch
// deleted, PR merged or closed) and "Update from main" (rebase or merge).
//
// The sidebar's counts come from a cheap `worktree_lifecycle` (no fetch, no
// gh) per visible repo: when the repo group mounts, whenever its worktree
// list changes (git moved a HEAD) and once a minute. The Clean up dialog asks
// again with `fetch` and `review` (uncommitted changes, pull requests). A
// daemon older than civetd 0.4.1 answers `unsupported`; that host is then
// left alone (no counts, no Clean up) while it runs that same daemon
// version; an upgrade, or a version not known yet, is asked again. Removal
// goes through the store's
// removeWorktree, one at a time, so git still refuses a dirty worktree and
// the sessions there are closed only after it succeeded.

import { useEffect } from 'react'
import { create } from 'zustand'
import { CivetError } from '@/backend'
import type {
  PaneInfo,
  RepoLifecycle,
  StaleReason,
  UpdateStrategy,
  WorktreeLifecycle,
  WorktreeUpdate
} from '@/types'
import {
  panesOfWorktree,
  pushToast,
  removeWorktree,
  selectWorktree,
  useStore,
  wsKey,
  wtKey
} from './store'
import { useChanges, refreshChanges } from './changes'
import { gitErrorMessage, setBusy } from './git-actions'

/** Sidebar refresh period per visible repo. */
export const LIFECYCLE_REFRESH_MS = 60_000

type LifecycleState = {
  /** wtKey(hostId, repo) → last cheap answer (no fetch, no review) */
  byRepo: Record<string, RepoLifecycle>
  /**
   * hostId → version of the daemon that answered `unsupported` (null: not
   * known when it answered, so it is asked again)
   */
  unsupported: Record<string, string | null>
  /** The Clean up dialog's repository, while it is open. */
  cleanup: { hostId: string; repo: string } | null
}

export const useLifecycle = create<LifecycleState>(() => ({
  byRepo: {},
  unsupported: {},
  cleanup: null
}))

const running = new Map<string, { again: boolean; done: Promise<void> }>()

/** Re-ask the cheap lifecycle of a repo (coalesced: one at a time, newest wins). */
export function refreshLifecycle(hostId: string, repo: string): Promise<void> {
  const b = useStore.getState().backend
  if (!b || !lifecycleSupported(hostId)) return Promise.resolve()
  const k = wtKey(hostId, repo)
  const r = running.get(k)
  if (r) {
    r.again = true
    return r.done
  }
  const entry = { again: true, done: Promise.resolve() }
  running.set(k, entry)
  entry.done = (async () => {
    try {
      while (entry.again) {
        entry.again = false
        const l = await b.worktreeLifecycle(hostId, repo, { fetch: false, review: false })
        useLifecycle.setState((s) => ({ byRepo: { ...s.byRepo, [k]: l } }))
      }
    } catch (err) {
      const e = CivetError.from(err)
      if (e.code === 'unsupported') {
        const version = useStore.getState().hosts[hostId]?.daemonVersion ?? null
        useLifecycle.setState((s) => ({ unsupported: { ...s.unsupported, [hostId]: version } }))
      } else {
        console.debug(`worktree_lifecycle ${repo} on ${hostId}:`, e)
      }
    } finally {
      running.delete(k)
    }
  })()
  return entry.done
}

/**
 * False while the host runs the daemon version that answered `unsupported`
 * (unknown versions and upgrades are asked again).
 */
export function lifecycleSupported(hostId: string): boolean {
  const refused = useLifecycle.getState().unsupported[hostId]
  return refused == null || refused !== useStore.getState().hosts[hostId]?.daemonVersion
}

/** {@link lifecycleSupported} as a hook. */
export function useLifecycleSupported(hostId: string): boolean {
  const refused = useLifecycle((l) => l.unsupported[hostId])
  const version = useStore((s) => s.hosts[hostId]?.daemonVersion)
  return refused == null || refused !== version
}

/**
 * Keep a repo's lifecycle fresh while its sidebar group is mounted and the
 * host connected: now, on every worktree re-list and once a minute.
 */
export function useRepoLifecycle(hostId: string, repo: string, connected: boolean): void {
  const list = useStore((s) => s.worktrees[wtKey(hostId, repo)])
  // biome-ignore lint/correctness/useExhaustiveDependencies: `list` changes on every re-list that moved a HEAD.
  useEffect(() => {
    if (!connected) return
    void refreshLifecycle(hostId, repo)
  }, [hostId, repo, connected, list])
  useEffect(() => {
    if (!connected) return
    const t = setInterval(() => {
      if (document.visibilityState === 'visible') void refreshLifecycle(hostId, repo)
    }, LIFECYCLE_REFRESH_MS)
    return () => clearInterval(t)
  }, [hostId, repo, connected])
}

/** The last known lifecycle of one worktree. */
export function useWorktreeLifecycle(
  hostId: string,
  repo: string,
  worktree: string
): { base: string | null; info: WorktreeLifecycle } | null {
  const l = useLifecycle((s) => s.byRepo[wtKey(hostId, repo)])
  const info = l?.worktrees.find((w) => w.worktree === worktree)
  return info ? { base: l!.base, info } : null
}

/** "↑2 ↓5"-style counts; '' when even. */
export function aheadBehindLabel(ahead: number, behind: number): string {
  return [ahead > 0 && `↑${ahead}`, behind > 0 && `↓${behind}`].filter(Boolean).join(' ')
}

/** "2 ahead of, 5 behind origin/main" for a tooltip. */
export function aheadBehindTitle(ahead: number, behind: number, base: string): string {
  if (!ahead && !behind) return `Even with ${base}`
  const parts = [ahead > 0 && `${ahead} ahead of`, behind > 0 && `${behind} behind`].filter(Boolean)
  return `${parts.join(', ')} ${base}`
}

/**
 * Rebase a branch nobody else has seen yet (never pushed, or its remote
 * branch is gone); merge into a pushed one so the next push needs no force.
 */
export function defaultUpdateStrategy(l: WorktreeLifecycle | null | undefined): UpdateStrategy {
  return l?.upstream && !l.upstreamGone ? 'merge' : 'rebase'
}

// ── Update from main ──────────────────────────────────────────────────────

/** Toast for what `update_worktree` did; `branch` names the worktree's branch. */
export function updateToast(
  u: WorktreeUpdate,
  strategy: UpdateStrategy,
  branch: string
): { kind: 'info' | 'error'; title: string; body?: string } {
  const files = u.conflicts.join(', ')
  const n = u.conflicts.length
  const conflictCount = `${n} file${n === 1 ? '' : 's'}`
  switch (u.outcome) {
    case 'upToDate':
      return { kind: 'info', title: `${branch} is up to date with ${u.base}` }
    case 'updated':
      return {
        kind: 'info',
        title:
          strategy === 'rebase'
            ? `Rebased ${branch} onto ${u.base}`
            : `Merged ${u.base} into ${branch}`
      }
    case 'rebaseConflicts':
      return {
        kind: 'error',
        title: `Rebase onto ${u.base} would conflict in ${conflictCount}`,
        body: `Nothing was changed. Conflicts: ${files}. Merge instead to resolve them in Changes.`
      }
    case 'mergeConflicts':
      return {
        kind: 'error',
        title: `Merging ${u.base} stopped with conflicts in ${conflictCount}`,
        body: `Resolve them in Changes and commit (or run git merge --abort): ${files}`
      }
  }
}

/**
 * Bring the default branch into a worktree's branch. Shares the Changes
 * tab's one-action-at-a-time `busy` label, so its controls grey out
 * meanwhile. Conflicts arrive as a toast: a refused rebase offers to merge
 * instead, a stopped merge opens Changes.
 */
export async function updateFromMain(
  hostId: string,
  worktree: string,
  strategy: UpdateStrategy
): Promise<WorktreeUpdate | undefined> {
  const s = useStore.getState()
  const b = s.backend
  if (!b) return undefined
  const key = wsKey(hostId, worktree)
  const wt = Object.values(s.worktrees)
    .flat()
    .find((w) => w.id === worktree)
  const branch = wt?.branch ?? 'this worktree'
  const what = strategy === 'rebase' ? 'Rebase' : 'Merge'
  const busy = useChanges.getState().busy[key]
  if (busy) {
    pushToast({ kind: 'info', title: `${what} from main: wait for "${busy}" to finish` }, 4000)
    return undefined
  }
  setBusy(key, strategy === 'rebase' ? 'Rebasing onto main…' : 'Merging main…')
  try {
    const u = await b.updateWorktree(hostId, worktree, strategy)
    const t = updateToast(u, strategy, branch)
    if (u.outcome === 'rebaseConflicts') {
      pushToast(
        {
          ...t,
          action: {
            label: 'Merge instead',
            run: () => void updateFromMain(hostId, worktree, 'merge')
          }
        },
        15_000
      )
    } else if (u.outcome === 'mergeConflicts') {
      pushToast(
        {
          ...t,
          action: {
            label: 'Show changes',
            run: () => {
              selectWorktree(hostId, worktree)
              useStore.setState({ rightPanelOpen: true, rightTab: 'changes' })
            }
          }
        },
        15_000
      )
    } else {
      pushToast(t, 3200)
    }
    return u
  } catch (err) {
    const e = CivetError.from(err)
    console.error(`[civet] ${what} from main failed`, e)
    pushToast(
      { kind: 'error', title: `Could not update ${branch}`, body: gitErrorMessage(e.message) },
      9000
    )
    return undefined
  } finally {
    setBusy(key, null)
    useChanges.setState((st) => ({
      diffEpoch: { ...st.diffEpoch, [key]: (st.diffEpoch[key] ?? 0) + 1 }
    }))
    void refreshChanges(hostId, worktree, 0, { force: true })
    if (wt) void refreshLifecycle(hostId, wt.repo)
  }
}

// ── Clean up worktrees ───────────────────────────────────────────────────

export const STALE_LABEL: Record<StaleReason, string> = {
  merged: 'Merged',
  upstreamGone: 'Remote branch deleted',
  pullRequestMerged: 'PR merged',
  pullRequestClosed: 'PR closed'
}

/** One stale worktree as the Clean up dialog lists it. */
export type CleanupCandidate = {
  info: WorktreeLifecycle
  /** Why it can't be removed from here (it would need force); null if it can. */
  blocked: string | null
  /** Reasons to look before removing it (unpushed commits, open sessions). */
  warnings: string[]
  /** Checked when the dialog opens. */
  preselect: boolean
}

/**
 * The stale worktrees of a review (`worktree_lifecycle` with `review`),
 * with what stands in the way. Blocked: uncommitted changes (git refuses
 * without force), an agent still working, a lock. Not preselected: commits
 * that are on no remote, unless a merged PR or merge says they landed.
 */
export function cleanupCandidates(
  l: RepoLifecycle,
  panes: Record<number, PaneInfo> | undefined,
  locked: (worktree: string) => boolean
): CleanupCandidate[] {
  return l.worktrees
    .filter((w) => w.stale.length > 0)
    .map((info) => {
      const live = panesOfWorktree(panes, info.worktree).filter((p) => !p.exited)
      const busy = live.filter(
        (p) => p.agent && (p.status === 'running' || p.status === 'needsInput')
      ).length
      const blocked = info.dirty
        ? `${info.dirty} uncommitted change${info.dirty === 1 ? '' : 's'}`
        : busy
          ? `${busy} agent${busy === 1 ? '' : 's'} still working`
          : locked(info.worktree)
            ? 'Locked with git worktree lock'
            : null
      const landed = info.stale.includes('merged') || info.stale.includes('pullRequestMerged')
      const warnings: string[] = []
      if (info.unpushed > 0 && !landed) {
        warnings.push(
          `${info.unpushed} commit${info.unpushed === 1 ? '' : 's'} on no remote branch`
        )
      }
      if (live.length > 0) {
        warnings.push(`${live.length} session${live.length === 1 ? '' : 's'} will be closed`)
      }
      return {
        info,
        blocked,
        warnings,
        preselect: !blocked && (landed || info.unpushed === 0)
      }
    })
}

/**
 * Remove the chosen worktrees one after another (the store's removeWorktree:
 * no force, so git still refuses a dirty one; its sessions close only after
 * it went). Failures are reported there; one summary toast for the rest.
 */
export async function cleanUpWorktrees(hostId: string, worktrees: string[]): Promise<number> {
  let removed = 0
  for (const w of worktrees) {
    if (await removeWorktree(hostId, w, false, { quiet: true })) removed++
  }
  if (removed > 0) {
    const host = useStore.getState().hosts[hostId]?.label ?? hostId
    pushToast(
      {
        kind: 'info',
        title: `Removed ${removed} worktree${removed === 1 ? '' : 's'} on ${host}`
      },
      3200
    )
  }
  return removed
}
