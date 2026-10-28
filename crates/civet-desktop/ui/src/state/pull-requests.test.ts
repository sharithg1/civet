import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { GitBranchInfo, PullRequestDetail, PullRequestInfo } from '../types'

beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore, wsKey, wtKey } = await import('./store')
const { useChanges } = await import('./changes')
const {
  checksSummary,
  createPullRequest,
  needsPush,
  refreshPullRequestDetail,
  reviewSummary,
  sortedChecks,
  usePullRequests
} = await import('./pull-requests')

const pr = (patch: Partial<PullRequestInfo> = {}): PullRequestInfo => ({
  number: 7,
  branch: 'feat',
  title: 'Feat',
  url: 'https://github.com/o/r/pull/7',
  state: 'open',
  ...patch
})

const detail = (patch: Partial<PullRequestDetail> = {}): PullRequestDetail => ({
  pr: pr(),
  base: 'main',
  checksState: null,
  checks: [],
  reviewDecision: null,
  approvals: 0,
  changesRequested: 0,
  comments: 0,
  unresolvedThreads: 0,
  threads: [],
  ...patch
})

const branch = (patch: Partial<GitBranchInfo> = {}): GitBranchInfo => ({
  branch: 'feat',
  head: 'abc',
  upstream: 'origin/feat',
  ahead: 0,
  behind: 0,
  canUndoCommit: false,
  stashes: 0,
  hasRemote: true,
  ...patch
})

function withBackend(b: Partial<CivetBackend>) {
  useStore.setState({
    backend: {
      gitStatus: vi.fn(async () => ({
        staged: [],
        unstaged: [],
        untracked: [],
        conflicted: [],
        truncated: false
      })),
      gitBranch: vi.fn(async () => branch()),
      pullRequestDetail: vi.fn(async () => null),
      ...b
    } as unknown as CivetBackend,
    toasts: [],
    repos: { h: [{ id: 'r', name: 'r', path: '/r', defaultBranch: 'main', kind: 'git' }] },
    worktrees: {
      [wtKey('h', 'r')]: [
        {
          id: '/w',
          repo: 'r',
          path: '/w',
          branch: 'feat',
          head: 'abc',
          isMain: false,
          locked: false
        }
      ]
    },
    pullRequests: {}
  })
}

beforeEach(() => {
  usePullRequests.setState({ byWorktree: {}, creating: {} })
  useChanges.setState({ byWorktree: {}, busy: {}, diffEpoch: {}, drafts: {} })
})

describe('pull requests', () => {
  it('pushes an unpushed branch before opening its PR, then lists it in the sidebar', async () => {
    const calls: string[] = []
    const gitAction = vi.fn(async () => {
      calls.push('push')
      return null
    })
    const created = pr({ number: 9, state: 'draft' })
    const create = vi.fn(async () => {
      calls.push('create')
      return created
    })
    withBackend({ gitAction, createPullRequest: create })
    const opts = { title: 'T', body: 'B', base: null, draft: true }
    expect(await createPullRequest('h', '/w', branch({ upstream: null }), opts)).toEqual(created)
    expect(calls).toEqual(['push', 'create'])
    expect(create).toHaveBeenCalledWith('h', { worktree: '/w', ...opts })
    expect(useStore.getState().pullRequests[wtKey('h', 'r')]?.feat).toEqual(created)
    expect(useStore.getState().toasts.at(-1)?.title).toBe('Opened draft PR #9')
    expect(usePullRequests.getState().creating[wsKey('h', '/w')]).toBeUndefined()

    // Pushed already: straight to gh.
    calls.length = 0
    await createPullRequest('h', '/w', branch(), opts)
    expect(calls).toEqual(['create'])
  })

  it('does not open the PR when the push fails, and reports gh refusals', async () => {
    const create = vi.fn(async () => pr())
    withBackend({
      gitAction: vi.fn(async () => {
        throw new CivetError('git', 'git: ! [rejected] feat -> feat (fetch first)')
      }),
      createPullRequest: create
    })
    const opts = { title: '', body: '', base: 'dev', draft: false }
    expect(await createPullRequest('h', '/w', branch({ ahead: 2 }), opts)).toBeNull()
    expect(create).not.toHaveBeenCalled()

    withBackend({
      createPullRequest: vi.fn(async () => {
        throw new CivetError('git', 'git: a pull request for branch "feat" already exists')
      })
    })
    expect(await createPullRequest('h', '/w', branch(), opts)).toBeNull()
    expect(useStore.getState().toasts.at(-1)).toMatchObject({
      kind: 'error',
      body: 'a pull request for branch "feat" already exists'
    })
  })

  it('keeps the last detail on a failure and syncs a newer PR into the sidebar', async () => {
    const d = detail({ pr: pr({ state: 'merged' }) })
    const pullRequestDetail = vi.fn(async () => d)
    withBackend({ pullRequestDetail })
    useStore.setState({ pullRequests: { [wtKey('h', 'r')]: { feat: pr() } } })
    await refreshPullRequestDetail('h', '/w')
    expect(pullRequestDetail).toHaveBeenCalledWith('h', '/w', false)
    const key = wsKey('h', '/w')
    expect(usePullRequests.getState().byWorktree[key]).toEqual({ detail: d, error: null })
    expect(useStore.getState().pullRequests[wtKey('h', 'r')]?.feat?.state).toBe('merged')

    // Throttled by minAgeMs.
    await refreshPullRequestDetail('h', '/w', 60_000)
    expect(pullRequestDetail).toHaveBeenCalledTimes(1)

    pullRequestDetail.mockRejectedValueOnce(new CivetError('spawn', 'spawn: gh not found'))
    await refreshPullRequestDetail('h', '/w')
    expect(usePullRequests.getState().byWorktree[key]).toMatchObject({
      detail: d,
      error: { code: 'spawn' }
    })
  })

  it('summarises checks and reviews', () => {
    const d = detail({
      checksState: 'failed',
      checks: [
        { name: 'b', state: 'passed', url: null },
        { name: 'a', state: 'passed', url: null },
        { name: 'z', state: 'failed', url: null },
        { name: 'bot', state: 'cancelled', url: null },
        { name: 'ci', state: 'pending', url: null }
      ],
      reviewDecision: 'changesRequested',
      approvals: 1,
      changesRequested: 1
    })
    expect(checksSummary(d)).toBe('1 failed, 1 pending, 2 passed, 1 cancelled')
    expect(sortedChecks(d).map((c) => c.name)).toEqual(['z', 'ci', 'a', 'b', 'bot'])
    expect(reviewSummary(d)).toBe('Changes requested · 1 approval')
    expect(reviewSummary(detail())).toBeNull()
    expect(reviewSummary(detail({ approvals: 2, changesRequested: 1 }))).toBe(
      '2 approvals · 1 requested changes'
    )
    expect(needsPush(branch())).toBe(false)
    expect(needsPush(branch({ ahead: 1 }))).toBe(true)
    expect(needsPush(branch({ upstream: null }))).toBe(true)
  })
})
