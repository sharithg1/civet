import { beforeAll, describe, expect, it, vi } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { GitFileChange, GitStatusInfo } from '../types'

beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore, wsKey } = await import('./store')
const { changedFileCount, findChange, refreshChanges, useChanges } = await import('./changes')

const change = (path: string, kind: GitFileChange['kind'] = 'modified'): GitFileChange => ({
  path,
  oldPath: null,
  kind,
  additions: 1,
  deletions: 0
})

const status = (patch: Partial<GitStatusInfo> = {}): GitStatusInfo => ({
  staged: [],
  unstaged: [],
  untracked: [],
  conflicted: [],
  truncated: false,
  ...patch
})

function withBackend(
  gitStatus: CivetBackend['gitStatus'],
  gitBranch: CivetBackend['gitBranch'] = async () => {
    throw new CivetError('unsupported', 'too old')
  }
) {
  useStore.setState({ backend: { gitStatus, gitBranch } as CivetBackend })
  useChanges.setState({ byWorktree: {} })
}

const branch = {
  branch: 'feat',
  head: 'abc',
  upstream: 'origin/feat',
  ahead: 1,
  behind: 0,
  canUndoCommit: true,
  stashes: 0,
  hasRemote: true
}

describe('changes', () => {
  it('counts each changed path once', () => {
    expect(changedFileCount(null)).toBe(0)
    const s = status({
      staged: [change('a')],
      unstaged: [change('a'), change('b')],
      untracked: [change('c', 'untracked')]
    })
    expect(changedFileCount(s)).toBe(3)
    expect(findChange(s, { side: 'unstaged', path: 'b', oldPath: null })?.path).toBe('b')
    expect(findChange(s, { side: 'staged', path: 'b', oldPath: null })).toBeUndefined()
  })

  it('stores a status, keeps an unchanged one and coalesces concurrent fetches', async () => {
    let resolve: (s: GitStatusInfo) => void = () => {}
    const gitStatus = vi.fn(() => new Promise<GitStatusInfo>((r) => (resolve = r)))
    withBackend(gitStatus)
    const first = refreshChanges('h', '/w')
    void refreshChanges('h', '/w')
    expect(gitStatus).toHaveBeenCalledTimes(1)
    resolve(status({ unstaged: [change('a')] }))
    await first
    const entry = useChanges.getState().byWorktree[wsKey('h', '/w')]!
    expect(changedFileCount(entry.status)).toBe(1)

    const again = refreshChanges('h', '/w')
    resolve(status({ unstaged: [change('a')] }))
    await again
    expect(useChanges.getState().byWorktree[wsKey('h', '/w')]).toBe(entry)
  })

  it('skips a poll when another poller fetched moments ago', async () => {
    const gitStatus = vi.fn(async () => status())
    withBackend(gitStatus)
    await refreshChanges('h', '/other', 1000)
    await refreshChanges('h', '/other', 1000)
    expect(gitStatus).toHaveBeenCalledTimes(1)
    await refreshChanges('h', '/other')
    expect(gitStatus).toHaveBeenCalledTimes(2)
  })

  it('keeps the last status when a fetch fails', async () => {
    withBackend(async () => status({ unstaged: [change('a')] }))
    await refreshChanges('h', '/w')
    useStore.setState({
      backend: {
        gitStatus: async () => {
          throw new CivetError('unsupported', 'too old')
        }
      } as unknown as CivetBackend
    })
    await refreshChanges('h', '/w')
    const entry = useChanges.getState().byWorktree[wsKey('h', '/w')]!
    expect(entry.error?.code).toBe('unsupported')
    expect(changedFileCount(entry.status)).toBe(1)
  })

  it('fetches the branch with the status and remembers a daemon without git actions', async () => {
    const gitBranch = vi.fn(async () => branch)
    withBackend(async () => status(), gitBranch)
    await refreshChanges('h', '/w')
    let entry = useChanges.getState().byWorktree[wsKey('h', '/w')]!
    expect(entry.branch).toEqual(branch)
    expect(entry.actions).toBe('supported')

    const old = vi.fn(async () => {
      throw new CivetError('unsupported', 'too old')
    })
    withBackend(async () => status(), old)
    await refreshChanges('h', '/w')
    await refreshChanges('h', '/w')
    entry = useChanges.getState().byWorktree[wsKey('h', '/w')]!
    expect(entry.actions).toBe('unsupported')
    expect(entry.status).not.toBeNull()
    // Asked once, then not again until the host reconnects.
    expect(old).toHaveBeenCalledTimes(1)
  })

  it('a forced refresh during a running fetch fetches again afterwards', async () => {
    const resolvers: ((s: GitStatusInfo) => void)[] = []
    const gitStatus = vi.fn(() => new Promise<GitStatusInfo>((r) => resolvers.push(r)))
    withBackend(gitStatus)
    const first = refreshChanges('h', '/w')
    await refreshChanges('h', '/w', 0, { force: true })
    expect(gitStatus).toHaveBeenCalledTimes(1)
    resolvers[0]!(status())
    await first
    await vi.waitFor(() => expect(gitStatus).toHaveBeenCalledTimes(2))
    resolvers[1]!(status({ staged: [change('a')] }))
    await vi.waitFor(() =>
      expect(useChanges.getState().byWorktree[wsKey('h', '/w')]?.status?.staged).toHaveLength(1)
    )
  })
})
