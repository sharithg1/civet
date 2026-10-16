import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { DirListing } from '../types'
import { setPrefs, usePrefs } from './prefs'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore, wsKey } = await import('./store')
const files = await import('./files')
const { useFiles } = files

const KEY = wsKey('h', '/w')

function listing(...names: string[]): DirListing {
  return {
    entries: names.map((n) => ({
      name: n.replace(/\/$/, ''),
      kind: n.endsWith('/') ? 'dir' : 'file',
      size: 1,
      ignored: false
    })),
    truncated: false
  }
}

/** A tree of `dir → listing`; any other dir is notFound. */
function backendWith(tree: Record<string, DirListing>) {
  const listFiles = vi.fn(async (_h: string, _w: string, dir: string) => {
    const l = tree[dir]
    if (!l) throw new CivetError('notFound', `"${dir}" not found`)
    return l
  })
  useStore.setState({ backend: { listFiles } as unknown as CivetBackend })
  return listFiles
}

beforeEach(() => {
  useFiles.setState({ dirs: {}, all: {}, previews: {} })
  setPrefs({ expandedDirs: {}, showIgnoredFiles: false })
})

describe('files store', () => {
  it('loads a level once and remembers expanded dirs per worktree', async () => {
    const listFiles = backendWith({ '': listing('src/', 'a'), src: listing('main.rs') })
    await files.loadDir('h', '/w', '')
    await files.loadDir('h', '/w', '')
    expect(listFiles).toHaveBeenCalledTimes(1)
    expect(listFiles).toHaveBeenCalledWith('h', '/w', '', false)

    files.toggleDir('h', '/w', 'src')
    await vi.waitFor(() => expect(useFiles.getState().dirs[KEY]?.src?.status).toBe('ok'))
    expect(usePrefs.getState().expandedDirs[KEY]).toEqual(['src'])
    files.toggleDir('h', '/w', 'src')
    expect(usePrefs.getState().expandedDirs[KEY]).toBeUndefined()
  })

  it('refresh reloads visible dirs and forgets deleted ones', async () => {
    const tree: Record<string, DirListing> = {
      '': listing('src/'),
      src: listing('old/'),
      'src/old': listing('x')
    }
    const listFiles = backendWith(tree)
    setPrefs({ expandedDirs: { [KEY]: ['src', 'src/old', 'hidden/below'] } })
    delete tree['src/old']
    await files.refreshFiles('h', '/w')
    // `hidden/below` is under a collapsed parent: not fetched.
    expect(listFiles.mock.calls.map((c) => c[2]).sort()).toEqual(['', 'src', 'src/old'])
    expect(usePrefs.getState().expandedDirs[KEY]).toEqual(['src', 'hidden/below'])
  })

  it('reopening a folder after a refresh loads its remembered expanded subfolders', async () => {
    const listFiles = backendWith({
      '': listing('src/'),
      src: listing('nested/'),
      'src/nested': listing('x')
    })
    const status = (d: string) => useFiles.getState().dirs[KEY]?.[d]?.status
    files.toggleDir('h', '/w', 'src')
    files.toggleDir('h', '/w', 'src/nested')
    await vi.waitFor(() => expect(status('src/nested')).toBe('ok'))
    files.toggleDir('h', '/w', 'src')
    await files.refreshFiles('h', '/w')
    expect(status('src/nested')).toBeUndefined()

    listFiles.mockClear()
    files.toggleDir('h', '/w', 'src')
    await vi.waitFor(() => expect(status('src/nested')).toBe('ok'))
    expect(listFiles.mock.calls.map((c) => c[2]).sort()).toEqual(['src', 'src/nested'])
  })

  it('reveals every ancestor of a picked file', () => {
    backendWith({})
    files.revealPath('h', '/w', 'a/b/c.txt')
    expect(usePrefs.getState().expandedDirs[KEY]?.sort()).toEqual(['a', 'a/b'])
  })

  it('drops a stale reply', async () => {
    let resolveFirst: (l: DirListing) => void = () => {}
    const listFiles = vi
      .fn()
      .mockImplementationOnce(() => new Promise<DirListing>((r) => (resolveFirst = r)))
      .mockResolvedValueOnce(listing('new'))
    useStore.setState({ backend: { listFiles } as unknown as CivetBackend })
    const first = files.loadDir('h', '/w', '')
    await files.loadDir('h', '/w', '', true)
    resolveFirst(listing('old'))
    await first
    const root = useFiles.getState().dirs[KEY]?.['']
    expect(root?.status === 'ok' && root.value.entries.map((e) => e.name)).toEqual(['new'])
  })
})

describe('paths', () => {
  it('joins and splits worktree-relative paths', () => {
    expect(files.joinPath('', 'a')).toBe('a')
    expect(files.joinPath('a/b', 'c')).toBe('a/b/c')
    expect(files.parentDir('a/b/c')).toBe('a/b')
    expect(files.parentDir('a')).toBe('')
  })

  it('lists visible rows in tree order for range selection', () => {
    const ok = (l: DirListing) => ({ status: 'ok' as const, value: l })
    const dirs = {
      '': ok(listing('src/', 'docs/', 'a')),
      src: ok(listing('bin/', 'main.rs')),
      'src/bin': ok(listing('x.rs')),
      docs: ok(listing('b.md'))
    }
    const rows = files.visibleRows(dirs, ['src', 'src/bin'])
    expect(rows.map((r) => `${r.depth}:${r.path}`)).toEqual([
      '0:src',
      '1:src/bin',
      '2:src/bin/x.rs',
      '1:src/main.rs',
      '0:docs',
      '0:a'
    ])
  })

  it('expanded folders follow a move, or are forgotten', () => {
    setPrefs({ expandedDirs: { [KEY]: ['src', 'src/bin', 'srcx', 'docs'] } })
    files.moveExpanded('h', '/w', 'src', 'lib/src')
    expect(usePrefs.getState().expandedDirs[KEY]).toEqual([
      'lib/src',
      'lib/src/bin',
      'srcx',
      'docs'
    ])
    files.moveExpanded('h', '/w', 'lib/src', null)
    expect(usePrefs.getState().expandedDirs[KEY]).toEqual(['srcx', 'docs'])
  })
})
