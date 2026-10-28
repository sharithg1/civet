import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { PaneInfo, RepoLifecycle, WorktreeLifecycle, WorktreeUpdate } from '../types'

beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true, visibilityState: 'visible' }
  })
})

const { useStore, wsKey, wtKey } = await import('./store')
const { useChanges } = await import('./changes')
const {
  aheadBehindLabel,
  aheadBehindTitle,
  cleanUpWorktrees,
  cleanupCandidates,
  defaultUpdateStrategy,
  refreshLifecycle,
  updateFromMain,
  updateToast,
  useLifecycle
} = await import('./lifecycle')

const life = (worktree: string, patch: Partial<WorktreeLifecycle> = {}): WorktreeLifecycle => ({
  worktree,
  branch: worktree.slice(1),
  ahead: 0,
  behind: 0,
  upstream: null,
  upstreamAhead: 0,
  upstreamBehind: 0,
  upstreamGone: false,
  unpushed: 0,
  dirty: 0,
  pullRequest: null,
  stale: [],
  ...patch
})

const pane = (id: number, worktree: string, patch: Partial<PaneInfo> = {}): PaneInfo =>
  ({
    id,
    worktree,
    exited: false,
    agent: null,
    status: 'unknown',
    ...patch
  }) as PaneInfo

beforeEach(() => {
  useChanges.setState({ byWorktree: {}, busy: {}, diffEpoch: {}, drafts: {} })
  useLifecycle.setState({ byRepo: {}, unsupported: {}, cleanup: null })
})

describe('worktree lifecycle', () => {
  it('labels ahead / behind', () => {
    expect(aheadBehindLabel(0, 0)).toBe('')
    expect(aheadBehindLabel(2, 0)).toBe('↑2')
    expect(aheadBehindLabel(2, 5)).toBe('↑2 ↓5')
    expect(aheadBehindTitle(2, 5, 'origin/main')).toBe('2 ahead of, 5 behind origin/main')
    expect(aheadBehindTitle(0, 1, 'main')).toBe('1 behind main')
    expect(aheadBehindTitle(0, 0, 'main')).toBe('Even with main')
  })

  it('rebases an unpushed branch and merges into a pushed one', () => {
    expect(defaultUpdateStrategy(life('/a'))).toBe('rebase')
    expect(defaultUpdateStrategy(life('/a', { upstream: 'origin/a' }))).toBe('merge')
    expect(defaultUpdateStrategy(life('/a', { upstream: 'origin/a', upstreamGone: true }))).toBe(
      'rebase'
    )
  })

  it('describes each update outcome', () => {
    const u = (outcome: WorktreeUpdate['outcome'], conflicts: string[] = []) => ({
      base: 'origin/main',
      outcome,
      conflicts
    })
    expect(updateToast(u('upToDate'), 'rebase', 'feat')).toEqual({
      kind: 'info',
      title: 'feat is up to date with origin/main'
    })
    expect(updateToast(u('updated'), 'rebase', 'feat').title).toBe('Rebased feat onto origin/main')
    expect(updateToast(u('updated'), 'merge', 'feat').title).toBe('Merged origin/main into feat')
    const r = updateToast(u('rebaseConflicts', ['a.rs', 'b.rs']), 'rebase', 'feat')
    expect(r.kind).toBe('error')
    expect(r.title).toBe('Rebase onto origin/main would conflict in 2 files')
    expect(r.body).toContain('Nothing was changed. Conflicts: a.rs, b.rs.')
    const m = updateToast(u('mergeConflicts', ['a.rs']), 'merge', 'feat')
    expect(m.title).toBe('Merging origin/main stopped with conflicts in 1 file')
    expect(m.body).toContain('Resolve them in Changes')
  })

  it('lists stale worktrees with what blocks or warns about each', () => {
    const l: RepoLifecycle = {
      base: 'origin/main',
      fetchError: null,
      pullRequestsError: null,
      worktrees: [
        life('/main'),
        life('/merged', { stale: ['merged'] }),
        life('/squashed', { stale: ['upstreamGone', 'pullRequestMerged'], unpushed: 3 }),
        life('/gone', { stale: ['upstreamGone'], unpushed: 2 }),
        life('/dirty', { stale: ['pullRequestClosed'], dirty: 4 }),
        life('/agent', { stale: ['merged'] }),
        life('/locked', { stale: ['merged'] })
      ]
    }
    const panes = {
      1: pane(1, '/agent', { agent: 'claude', status: 'running' }),
      2: pane(2, '/merged'),
      3: pane(3, '/merged', { exited: true })
    }
    const c = cleanupCandidates(l, panes, (w) => w === '/locked')
    const by = Object.fromEntries(c.map((x) => [x.info.worktree, x]))
    expect(Object.keys(by)).toEqual([
      '/merged',
      '/squashed',
      '/gone',
      '/dirty',
      '/agent',
      '/locked'
    ])
    expect(by['/merged']).toMatchObject({
      blocked: null,
      preselect: true,
      warnings: ['1 session will be closed']
    })
    // Squash-merged: its commits are on no remote, but the PR landed them.
    expect(by['/squashed']).toMatchObject({ blocked: null, preselect: true, warnings: [] })
    expect(by['/gone']).toMatchObject({
      blocked: null,
      preselect: false,
      warnings: ['2 commits on no remote branch']
    })
    expect(by['/dirty']).toMatchObject({ blocked: '4 uncommitted changes', preselect: false })
    expect(by['/agent']).toMatchObject({ blocked: '1 agent still working', preselect: false })
    expect(by['/locked']).toMatchObject({ blocked: 'Locked with git worktree lock' })
  })

  it('remembers a daemon too old for lifecycle and stops asking it', async () => {
    const worktreeLifecycle = vi.fn(async () => {
      throw new CivetError('unsupported', 'civetd 0.3.0')
    })
    useStore.setState({ backend: { worktreeLifecycle } as unknown as CivetBackend })
    await refreshLifecycle('h', 'r')
    expect(useLifecycle.getState().unsupported).toEqual({ h: true })
    await refreshLifecycle('h', 'r')
    expect(worktreeLifecycle).toHaveBeenCalledTimes(1)
  })

  it('stores the cheap lifecycle, coalescing overlapping refreshes', async () => {
    let calls = 0
    const answer: RepoLifecycle = {
      base: 'main',
      worktrees: [life('/a', { ahead: 1 })],
      fetchError: null,
      pullRequestsError: null
    }
    const worktreeLifecycle = vi.fn(async () => {
      calls++
      return answer
    })
    useStore.setState({ backend: { worktreeLifecycle } as unknown as CivetBackend })
    await Promise.all([refreshLifecycle('h', 'r'), refreshLifecycle('h', 'r')])
    expect(calls).toBe(2)
    expect(worktreeLifecycle).toHaveBeenCalledWith('h', 'r', { fetch: false, review: false })
    expect(useLifecycle.getState().byRepo[wtKey('h', 'r')]).toBe(answer)
  })

  it('updates from main busy-labelled, offering a merge after a refused rebase', async () => {
    let release: (u: WorktreeUpdate) => void = () => {}
    const updateWorktree = vi.fn(
      () => new Promise<WorktreeUpdate>((r) => (release = r))
    ) as unknown as CivetBackend['updateWorktree']
    const gitStatus = vi.fn(async () => ({
      staged: [],
      unstaged: [],
      untracked: [],
      conflicted: [],
      truncated: false
    }))
    const gitBranch = vi.fn(async () => {
      throw new CivetError('unsupported', 'old')
    })
    const worktreeLifecycle = vi.fn(async () => ({
      base: 'origin/main',
      worktrees: [],
      fetchError: null,
      pullRequestsError: null
    }))
    useStore.setState({
      backend: {
        updateWorktree,
        gitStatus,
        gitBranch,
        worktreeLifecycle
      } as unknown as CivetBackend,
      worktrees: {
        [wtKey('h', 'r')]: [
          {
            id: '/w',
            repo: 'r',
            path: '/w',
            branch: 'feat',
            head: null,
            isMain: false,
            locked: false
          }
        ]
      },
      toasts: []
    })
    const key = wsKey('h', '/w')
    const p = updateFromMain('h', '/w', 'rebase')
    expect(useChanges.getState().busy[key]).toBe('Rebasing onto main…')
    // One git action at a time.
    expect(await updateFromMain('h', '/w', 'merge')).toBeUndefined()
    expect(updateWorktree).toHaveBeenCalledTimes(1)
    release({ base: 'origin/main', outcome: 'rebaseConflicts', conflicts: ['a.rs'] })
    await p
    expect(useChanges.getState().busy[key]).toBeUndefined()
    expect(useChanges.getState().diffEpoch[key]).toBe(1)
    expect(worktreeLifecycle).toHaveBeenCalled()
    const t = useStore.getState().toasts.at(-1)!
    expect(t.kind).toBe('error')
    expect(t.action?.label).toBe('Merge instead')
    t.action!.run()
    expect(updateWorktree).toHaveBeenLastCalledWith('h', '/w', 'merge')
    release({ base: 'origin/main', outcome: 'updated', conflicts: [] })
    await vi.waitFor(() =>
      expect(useStore.getState().toasts.at(-1)?.title).toBe('Merged origin/main into feat')
    )
  })

  it('reports a refused update with the daemon’s reason', async () => {
    const updateWorktree = vi.fn(async () => {
      throw new CivetError(
        'invalidArgument',
        'invalid argument: there are uncommitted changes; commit or stash them first'
      )
    })
    useStore.setState({
      backend: {
        updateWorktree,
        gitStatus: vi.fn(async () => null),
        gitBranch: vi.fn(async () => null),
        worktreeLifecycle: vi.fn(async () => null)
      } as unknown as CivetBackend,
      worktrees: {},
      toasts: []
    })
    expect(await updateFromMain('h', '/w', 'merge')).toBeUndefined()
    expect(useStore.getState().toasts.at(-1)).toMatchObject({
      kind: 'error',
      title: 'Could not update this worktree',
      body: 'there are uncommitted changes; commit or stash them first'
    })
  })

  it('cleans up one worktree at a time and sums up what went', async () => {
    const order: string[] = []
    const gone = new Set<string>()
    const removeWorktree = vi.fn(async (_h: string, w: string, force: boolean) => {
      order.push(`${w}:${force}`)
      if (w === '/dirty') throw new CivetError('git', 'contains modified or untracked files')
      gone.add(w)
    })
    const wt = (id: string) => ({
      id,
      repo: 'r',
      path: id,
      branch: id.slice(1),
      head: null,
      isMain: false,
      locked: false
    })
    useStore.setState({
      backend: {
        removeWorktree,
        killPane: vi.fn(async () => {}),
        listWorktrees: vi.fn(async () =>
          [wt('/a'), wt('/dirty'), wt('/b')].filter((w) => !gone.has(w.id))
        ),
        listPullRequests: vi.fn(async () => [])
      } as unknown as CivetBackend,
      hosts: { h: { hostId: 'h', label: 'devbox' } as never },
      repos: { h: [{ id: 'r', name: 'r', path: '/r', defaultBranch: 'main' }] },
      worktrees: { [wtKey('h', 'r')]: [wt('/a'), wt('/dirty'), wt('/b')] },
      panes: {},
      toasts: []
    })
    expect(await cleanUpWorktrees('h', ['/a', '/dirty', '/b'])).toBe(2)
    expect(order).toEqual(['/a:false', '/dirty:false', '/b:false'])
    const titles = useStore.getState().toasts.map((t) => t.title)
    expect(titles).toContain('Could not remove dirty')
    expect(titles.at(-1)).toBe('Removed 2 worktrees on devbox')
    expect(titles.filter((t) => t.startsWith('Removed '))).toHaveLength(1)
  })
})
