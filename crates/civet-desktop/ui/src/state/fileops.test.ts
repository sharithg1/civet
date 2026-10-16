import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { MockBackend } from '../backend.mock'
import { setPrefs, usePrefs } from './prefs'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore, wsKey, openFileTab } = await import('./store')
const files = await import('./files')
const ops = await import('./fileops')

const W = '/w'
const KEY = wsKey('devbox', W)

async function setup() {
  const b = new MockBackend()
  vi.useFakeTimers()
  const p = b.connectHost({ kind: 'ssh', destination: 'devbox' })
  await vi.runAllTimersAsync()
  const summary = await p
  vi.useRealTimers()
  useStore.setState({
    backend: b,
    hosts: {
      devbox: { ...summary, rttMs: null, message: null, bootstrap: null, errorCode: null }
    },
    workspaces: {},
    toasts: []
  })
  await files.loadDir('devbox', W, '')
  return b
}

const names = (dir: string) => {
  const l = files.useFiles.getState().dirs[KEY]?.[dir]
  return l?.status === 'ok' ? l.value.entries.map((e) => e.name) : null
}
const tabPaths = () =>
  useStore.getState().workspaces[KEY]?.tabs.map((t) => (t.kind === 'file' ? t.path : '?')) ?? []

beforeEach(() => {
  files.useFiles.setState({ dirs: {}, all: {}, previews: {} })
  setPrefs({ expandedDirs: {}, showIgnoredFiles: false })
  ops.setFileClip(null)
})

describe('file op helpers', () => {
  it('names copies like Finder', () => {
    expect(ops.copyName('a.txt', new Set())).toBe('a copy.txt')
    expect(ops.copyName('a.txt', new Set(['a copy.txt', 'a copy 2.txt']))).toBe('a copy 3.txt')
    expect(ops.copyName('.env', new Set())).toBe('.env copy')
    expect(ops.copyName('lib.v2', new Set(), true)).toBe('lib.v2 copy')
  })

  it('rejects names the daemon would', () => {
    for (const bad of ['', '  ', 'a/b', '.', '..', '.git', '.GIT'])
      expect(ops.nameError(bad)).not.toBeNull()
    expect(ops.nameError('.gitignore')).toBeNull()
  })

  it('keeps only top-level paths', () => {
    expect(ops.topLevel(['a', 'a/b', 'ab', 'c/d', 'c/d/e', 'a'])).toEqual(['a', 'ab', 'c/d'])
  })

  it('gates on the daemon version', () => {
    expect(ops.versionAtLeast('0.1.14', '0.1.14')).toBe(true)
    expect(ops.versionAtLeast('0.2.0', '0.1.14')).toBe(true)
    expect(ops.versionAtLeast('0.1.13', '0.1.14')).toBe(false)
    expect(ops.versionAtLeast('0.1.9', '0.1.14')).toBe(false)
    useStore.setState({
      hosts: { h: { daemonVersion: '0.1.13' } as never, n: { daemonVersion: '0.1.14' } as never }
    })
    expect(ops.fileOpsSupported('h')).toBe(false)
    expect(ops.fileOpsSupported('n')).toBe(true)
    expect(ops.fileOpsSupported('missing')).toBe(false)
  })
})

describe('file ops', () => {
  it('creates, opens and lists a new file right away', async () => {
    await setup()
    expect(await ops.createEntry('devbox', W, 'docs', 'new.md', 'file')).toBe('docs/new.md')
    expect(usePrefs.getState().expandedDirs[KEY]).toContain('docs')
    await vi.waitFor(() => expect(names('docs')).toContain('new.md'))
    expect(tabPaths()).toEqual(['docs/new.md'])
  })

  it('renames and moves: tabs and expanded folders follow', async () => {
    await setup()
    openFileTab('devbox', W, 'crates/civetd/src/main.rs')
    files.expandDir('devbox', W, 'crates/civetd/src')
    await ops.renameEntry('devbox', W, 'crates/civetd', 'daemon')
    expect(tabPaths()).toEqual(['crates/daemon/src/main.rs'])
    expect(usePrefs.getState().expandedDirs[KEY]).toEqual(
      expect.arrayContaining(['crates', 'crates/daemon', 'crates/daemon/src'])
    )
    await ops.moveEntries('devbox', W, ['crates/daemon', 'crates/daemon/src'], '')
    expect(tabPaths()).toEqual(['daemon/src/main.rs'])
    expect(names('')).toContain('daemon')
    expect(names('crates')).not.toContain('daemon')
  })

  it('duplicates next to itself and pastes a copy or a cut', async () => {
    const b = await setup()
    expect(await ops.duplicateEntry('devbox', W, 'README.md')).toBe('README copy.md')
    expect(await ops.duplicateEntry('devbox', W, 'README.md')).toBe('README copy 2.md')
    ops.setFileClip({ hostId: 'devbox', worktree: W, paths: ['Cargo.toml'], mode: 'copy' })
    await ops.pasteEntries('devbox', W, 'docs')
    await ops.pasteEntries('devbox', W, 'docs')
    expect(b.files('devbox', W).files.has('docs/Cargo.toml')).toBe(true)
    expect(b.files('devbox', W).files.has('docs/Cargo copy.toml')).toBe(true)
    ops.setFileClip({ hostId: 'devbox', worktree: W, paths: ['README.md'], mode: 'cut' })
    await ops.pasteEntries('devbox', W, 'scripts')
    expect(ops.useFileClipboard.getState().clip).toBeNull()
    expect(b.files('devbox', W).files.has('scripts/README.md')).toBe(true)
    expect(b.files('devbox', W).files.has('README.md')).toBe(false)
  })

  it('deletes to the trash, closes tabs, and Undo restores', async () => {
    const b = await setup()
    openFileTab('devbox', W, 'docs/INTERFACES.md')
    await ops.trashEntries('devbox', W, ['docs', 'docs/logo.png', 'Cargo.toml'])
    expect(tabPaths()).toEqual([])
    expect(names('')).not.toContain('docs')
    const toast = useStore.getState().toasts.at(-1)!
    expect(toast.title).toBe('Deleted 2 items')
    toast.action!.run()
    await vi.waitFor(() =>
      expect(names('')).toEqual(expect.arrayContaining(['docs', 'Cargo.toml']))
    )
    expect(b.files('devbox', W).files.has('docs/logo.png')).toBe(true)
  })

  it('reports a refused op as a readable toast', async () => {
    await setup()
    expect(await ops.renameEntry('devbox', W, 'Cargo.toml', 'README.md')).toBeNull()
    const toast = useStore.getState().toasts.at(-1)!
    expect(toast).toMatchObject({ kind: 'error', title: 'Could not rename Cargo.toml' })
    expect(toast.body).toMatch(/already exists/)
    expect(await ops.moveEntries('devbox', W, ['docs'], 'docs/x')).toEqual([])
    expect(useStore.getState().toasts.at(-1)!.title).toBe('Could not move docs')
  })
})
