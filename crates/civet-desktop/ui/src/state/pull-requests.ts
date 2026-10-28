// The pull request of a worktree's branch: CI checks, reviews and unresolved
// review threads (`pull_request_detail`: `gh api graphql` on the host,
// civetd >= 0.4.3), and opening one from the Changes tab
// (`create_pull_request`: `gh pr create`).
//
// Polled like git status (state/changes.ts): every 30 s while the Changes
// tab shows the worktree, every 2 min for sidebar rows whose branch has an
// open PR; never while the window is hidden or the host is not connected.
// Pollers of a worktree share fetches, and while any of them wants the
// review threads every fetch includes them. A PR seen here also updates the
// sidebar's `gh pr list` entry (store.pullRequests), so a new or merged PR
// shows there without waiting for the next list.

import { useEffect } from 'react'
import { create } from 'zustand'
import { CivetError } from '@/backend'
import type { GitBranchInfo, PullRequestDetail, PullRequestInfo } from '@/types'
import { pushToast, useStore, wsKey, wtKey } from './store'
import { gitErrorMessage, runGitAction } from './git-actions'

/** Poll period while the Changes tab shows the worktree. */
export const PR_DETAIL_POLL_MS = 30_000
/** Poll period of a sidebar row with an open PR. */
export const PR_DETAIL_BACKGROUND_POLL_MS = 120_000

export type PullRequestEntry = {
  /** null: the branch has no PR (or the worktree no branch). */
  detail: PullRequestDetail | null
  /** Last failure (cleared by the next success): no gh, not logged in, older civetd… */
  error: CivetError | null
}

type PullRequestsState = {
  /** wsKey(hostId, worktree) → last answer; missing until the first one. */
  byWorktree: Record<string, PullRequestEntry>
  /** wsKey → a PR is being opened there. */
  creating: Record<string, boolean>
}

export const usePullRequests = create<PullRequestsState>(() => ({
  byWorktree: {},
  creating: {}
}))

const inFlight = new Set<string>()
const queued = new Set<string>()
/** wsKey → when its last fetch started (ms). */
const fetchedAt = new Map<string, number>()
/** wsKey → pollers that want the review threads. */
const wantThreads = new Map<string, number>()

function setEntry(key: string, entry: PullRequestEntry): void {
  usePullRequests.setState((s) => ({ byWorktree: { ...s.byWorktree, [key]: entry } }))
}

/**
 * Fetch a worktree's PR once: a no-op while a fetch for it runs or if one
 * started less than `minAgeMs` ago. `force`: if one is running, fetch again
 * after it (it may predate a push or a new PR).
 */
export async function refreshPullRequestDetail(
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
    const detail = await b.pullRequestDetail(hostId, worktree, (wantThreads.get(key) ?? 0) > 0)
    const next: PullRequestEntry = { detail, error: null }
    if (detail) syncListedPullRequest(hostId, worktree, detail.pr)
    const latest = usePullRequests.getState().byWorktree[key]
    // Why: polls mostly return the same thing; keep the object so nothing re-renders.
    if (latest && JSON.stringify(latest) === JSON.stringify(next)) return
    setEntry(key, next)
  } catch (err) {
    const prev = usePullRequests.getState().byWorktree[key]
    setEntry(key, { detail: prev?.detail ?? null, error: CivetError.from(err) })
  } finally {
    inFlight.delete(key)
    if (queued.delete(key)) void refreshPullRequestDetail(hostId, worktree, 0, { force: true })
  }
}

/** Put `pr` into the sidebar's per-branch PR map if it is newer or changed. */
function syncListedPullRequest(hostId: string, worktree: string, pr: PullRequestInfo): void {
  const s = useStore.getState()
  const repo = (s.repos[hostId] ?? []).find((r) =>
    s.worktrees[wtKey(hostId, r.id)]?.some((w) => w.id === worktree)
  )
  if (!repo) return
  const k = wtKey(hostId, repo.id)
  const listed = s.pullRequests[k]?.[pr.branch]
  if (listed && (listed.number > pr.number || JSON.stringify(listed) === JSON.stringify(pr))) return
  useStore.setState((st) => ({
    pullRequests: { ...st.pullRequests, [k]: { ...st.pullRequests[k], [pr.branch]: pr } }
  }))
}

/**
 * Poll a worktree's PR every `intervalMs` while `enabled`, the host is
 * connected and the window visible (and right away when it becomes so).
 * `threads`: this poller shows the review threads. Stops after an
 * `unsupported` error (an older civetd) until the host connects again.
 */
export function usePullRequestPolling(
  hostId: string,
  worktree: string,
  intervalMs: number,
  { threads = false, enabled = true }: { threads?: boolean; enabled?: boolean } = {}
): void {
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  useEffect(() => {
    if (!connected || !enabled) return
    const key = wsKey(hostId, worktree)
    if (threads) wantThreads.set(key, (wantThreads.get(key) ?? 0) + 1)
    const entry = usePullRequests.getState().byWorktree[key]
    const retry = entry?.error?.code === 'unsupported'
    if (retry) setEntry(key, { detail: null, error: null })
    // Threads wanted but the last answer has none: fetch now.
    const stale =
      threads &&
      !!entry?.detail &&
      entry.detail.unresolvedThreads > 0 &&
      entry.detail.threads.length === 0
    const tick = (now = false) => {
      if (document.visibilityState === 'hidden') return
      if (usePullRequests.getState().byWorktree[key]?.error?.code === 'unsupported') return
      void refreshPullRequestDetail(hostId, worktree, now ? 0 : intervalMs / 2)
    }
    tick(retry || stale)
    const onTick = () => tick()
    const timer = setInterval(onTick, intervalMs)
    document.addEventListener('visibilitychange', onTick)
    return () => {
      clearInterval(timer)
      document.removeEventListener('visibilitychange', onTick)
      if (threads) wantThreads.set(key, (wantThreads.get(key) ?? 1) - 1)
    }
  }, [hostId, worktree, intervalMs, threads, enabled, connected])
}

/** The branch has commits or a branch the remote does not have yet. */
export const needsPush = (branch: GitBranchInfo): boolean => !branch.upstream || branch.ahead > 0

/**
 * Open a PR for the worktree's branch, pushing (publishing) it first if the
 * remote lacks commits. Resolves to the PR, or null if it failed (already
 * reported as a toast).
 */
export async function createPullRequest(
  hostId: string,
  worktree: string,
  branch: GitBranchInfo,
  opts: { title: string; body: string; base: string | null; draft: boolean }
): Promise<PullRequestInfo | null> {
  const b = useStore.getState().backend
  const key = wsKey(hostId, worktree)
  if (!b || usePullRequests.getState().creating[key]) return null
  const setCreating = (on: boolean) =>
    usePullRequests.setState((s) => {
      const creating = { ...s.creating }
      if (on) creating[key] = true
      else delete creating[key]
      return { creating }
    })
  setCreating(true)
  try {
    if (needsPush(branch)) {
      const name = branch.branch ?? 'the branch'
      const pushed = await runGitAction(hostId, worktree, 'push', {
        busy: branch.upstream ? 'Pushing…' : 'Publishing…',
        failed: branch.upstream ? 'Push failed' : 'Could not publish the branch',
        done: branch.upstream ? `Pushed ${name}` : `Published ${name}`
      })
      if (pushed === undefined) return null
    }
    const pr = await b.createPullRequest(hostId, { worktree, ...opts })
    syncListedPullRequest(hostId, worktree, pr)
    pushToast(
      {
        kind: 'info',
        title: `Opened ${pr.state === 'draft' ? 'draft ' : ''}PR #${pr.number}`,
        body: pr.title,
        action: { label: 'Open', run: () => void openUrl(pr.url) }
      },
      8000
    )
    return pr
  } catch (err) {
    const e = CivetError.from(err)
    console.error('[civet] Could not open the pull request', e)
    pushToast(
      { kind: 'error', title: 'Could not open the pull request', body: gitErrorMessage(e.message) },
      9000
    )
    return null
  } finally {
    setCreating(false)
    void refreshPullRequestDetail(hostId, worktree, 0, { force: true })
  }
}

/** Open a PR, check or review comment in the browser. */
export async function openUrl(url: string): Promise<void> {
  try {
    await useStore.getState().backend?.openWebUrl(url)
  } catch (err) {
    pushToast(
      { kind: 'error', title: 'Could not open the link', body: CivetError.from(err).message },
      6000
    )
  }
}

/** "3 passed, 1 failed, 1 pending" for the checks of a PR, failures first. */
export function checksSummary(detail: PullRequestDetail): string {
  const order = ['failed', 'pending', 'passed', 'skipped', 'cancelled'] as const
  const counts = new Map<string, number>()
  for (const c of detail.checks) counts.set(c.state, (counts.get(c.state) ?? 0) + 1)
  const parts = order.filter((s) => counts.has(s)).map((s) => `${counts.get(s)} ${s}`)
  return parts.length > 0 ? parts.join(', ') : 'No checks'
}

/** "Changes requested", "Approved"…, with reviewer counts. */
export function reviewSummary(detail: PullRequestDetail): string | null {
  const parts: string[] = []
  if (detail.reviewDecision === 'approved') parts.push('Approved')
  else if (detail.reviewDecision === 'changesRequested') parts.push('Changes requested')
  else if (detail.reviewDecision === 'reviewRequired') parts.push('Review required')
  const n = (k: number, one: string) => `${k} ${one}${k === 1 ? '' : 's'}`
  if (detail.approvals > 0 && detail.reviewDecision !== 'approved')
    parts.push(n(detail.approvals, 'approval'))
  if (detail.changesRequested > 0 && detail.reviewDecision !== 'changesRequested') {
    parts.push(`${detail.changesRequested} requested changes`)
  }
  return parts.length > 0 ? parts.join(' · ') : null
}

/** Checks failing first, then running, then the rest; by name within each. */
export function sortedChecks(detail: PullRequestDetail): PullRequestDetail['checks'] {
  const rank = { failed: 0, pending: 1, passed: 2, skipped: 3, cancelled: 4 }
  return [...detail.checks].sort(
    (a, b) => rank[a.state] - rank[b.state] || a.name.localeCompare(b.name)
  )
}
