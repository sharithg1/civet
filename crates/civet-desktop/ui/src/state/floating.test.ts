import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import type { CivetBackend } from '../backend'
import type { HostState, RepoInfo, WorktreeInfo } from '../types'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const store = await import('./store')
const floating = await import('./floating')
const { useStore } = store

const DIR = '/home/dev/.civet/workspaces/release'

function host(daemonVersion: string | null, state: HostState = 'connected') {
  return {
    hostId: 'h',
    label: 'h',
    state,
    transport: 'quic' as const,
    daemonVersion,
    instanceId: '1',
    upgrade: null,
    rttMs: null,
    message: null,
    bootstrap: null,
    errorCode: null
  }
}

/** A backend holding one host's workspaces, like civetd: one worktree each. */
function workspaceBackend() {
  const repos: RepoInfo[] = []
  const createWorkspace = vi.fn(async (_h: string, name: string, roots: string[]) => {
    const ws: RepoInfo = {
      id: 'w',
      name,
      path: DIR,
      defaultBranch: null,
      kind: 'workspace',
      roots: roots.map((path) => ({
        path,
        name: path.split('/').pop()!,
        isDir: true,
        missing: false
      }))
    }
    repos.push(ws)
    return ws
  })
  const setWorkspaceRoots = vi.fn(async (_h: string, id: string, roots: string[]) => {
    const i = repos.findIndex((r) => r.id === id)
    repos[i] = {
      ...repos[i]!,
      roots: roots.map((path) => ({ path, name: path, isDir: true, missing: false }))
    }
    return repos[i]!
  })
  const listFiles = vi.fn(async () => ({ entries: [], truncated: false }))
  const backend = {
    kind: 'mock',
    listRepos: async () => [...repos],
    listWorktrees: async (_h: string, repo: string): Promise<WorktreeInfo[]> =>
      repos.some((r) => r.id === repo)
        ? [{ id: DIR, repo, path: DIR, branch: null, head: null, isMain: true, locked: false }]
        : [],
    listPullRequests: async () => [],
    listFiles,
    listAllFiles: async () => ({ paths: [], truncated: false }),
    createWorkspace,
    setWorkspaceRoots
  } as unknown as CivetBackend
  return { backend, createWorkspace, setWorkspaceRoots, listFiles }
}

beforeEach(() => {
  floating.closeWorkspaceDialog()
})

describe('floating workspaces', () => {
  it('needs a connected host with civetd 0.6.3', () => {
    expect(floating.hasFloatingWorkspaces(undefined)).toBe(false)
    expect(floating.hasFloatingWorkspaces(host('0.6.2'))).toBe(false)
    expect(floating.hasFloatingWorkspaces(host('0.6.3'))).toBe(true)
    expect(floating.hasFloatingWorkspaces(host('0.7.0'))).toBe(true)
    expect(floating.hasFloatingWorkspaces(host('0.7.0', 'reconnecting'))).toBe(false)
  })

  it('cleans the roots a form holds', () => {
    expect(floating.cleanRoots([' ~/a ', '', '~/b', '~/a', '   '])).toEqual(['~/a', '~/b'])
  })

  it('creates a workspace, lists it and opens its folder; then replaces its roots', async () => {
    const b = workspaceBackend()
    useStore.setState({
      backend: b.backend,
      hosts: { h: host('0.6.3') },
      hostOrder: ['h'],
      repos: { h: [] },
      worktrees: {},
      panes: { h: {} },
      workspaces: {},
      active: null
    })
    await floating.createFloatingWorkspace('h', ' release ', ['~/src/a ', '', '~/src/b', '~/src/a'])
    expect(b.createWorkspace).toHaveBeenCalledWith('h', 'release', ['~/src/a', '~/src/b'])
    const s = useStore.getState()
    expect(s.repos.h!.map((r) => [r.id, r.kind])).toEqual([['w', 'workspace']])
    expect(s.active).toEqual({ hostId: 'h', worktree: DIR })
    expect(store.isFolderWorktree('h', DIR)).toBe(true)

    floating.openWorkspaceDialog('h', 'w')
    expect(floating.useWorkspaceDialog.getState().request).toEqual({ hostId: 'h', repo: 'w' })
    b.listFiles.mockClear()
    await floating.setFloatingWorkspaceRoots('h', 'w', ['/srv/c'])
    expect(b.setWorkspaceRoots).toHaveBeenCalledWith('h', 'w', ['/srv/c'])
    expect(useStore.getState().repos.h![0]!.roots!.map((r) => r.path)).toEqual(['/srv/c'])
    // The links changed: the Files tab reloads the folder.
    await vi.waitFor(() =>
      expect(b.listFiles).toHaveBeenCalledWith('h', DIR, '', expect.anything())
    )
  })

  it('a refused create leaves the store alone', async () => {
    const b = workspaceBackend()
    b.createWorkspace.mockRejectedValueOnce(new Error('~/nope does not exist'))
    useStore.setState({
      backend: b.backend,
      hosts: { h: host('0.6.3') },
      repos: { h: [] },
      worktrees: {},
      active: null
    })
    await expect(floating.createFloatingWorkspace('h', 'x', ['~/nope'])).rejects.toThrow(
      'does not exist'
    )
    expect(useStore.getState().repos.h).toEqual([])
    expect(useStore.getState().active).toBeNull()
  })
})
