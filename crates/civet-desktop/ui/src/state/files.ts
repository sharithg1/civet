// Files tab (right panel): a worktree's directory tree, loaded one level at
// a time (`list_files`), the flat file list for quick-open (`list_all_files`)
// and the contents of files open in tabs (`read_file`, see FileView.tsx).
//
// Expanded directories persist per worktree in prefs; listings are a cache
// that `refreshFiles` reloads (manual refresh, window focus). A refresh keeps
// showing the old listing until the new one arrives, and forgets expanded
// directories that no longer exist.

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import type { DirListing, FileList, FilePreview } from '@/types'
import { setPrefs, usePrefs } from './prefs'
import { useStore, wsKey } from './store'

/** Preview size cap (the daemon allows up to 1 MiB). */
export const PREVIEW_BYTES = 256 * 1024

export type Loadable<T> =
  | { status: 'loading' }
  | { status: 'ok'; value: T }
  | { status: 'error'; code: string; message: string }

type FilesState = {
  /** wsKey → directory ('' = root) → listing */
  dirs: Record<string, Record<string, Loadable<DirListing>>>
  /** wsKey → every file, for quick-open */
  all: Record<string, Loadable<FileList>>
  /** `fileKey(hostId, worktree, path)` → contents of a file open in a tab */
  previews: Record<string, Loadable<FilePreview>>
}

export const useFiles = create<FilesState>(() => ({ dirs: {}, all: {}, previews: {} }))

function b(): CivetBackend {
  const be = useStore.getState().backend
  if (!be) throw new Error('backend not initialised')
  return be
}

type Failed = Extract<Loadable<never>, { status: 'error' }>

function failed(err: unknown): Failed {
  const e = CivetError.from(err)
  return { status: 'error', code: e.code, message: e.message }
}

/** Latest request per slot: older replies are dropped. */
const latest = new Map<string, number>()
let seq = 0

function begin(slot: string): () => boolean {
  const n = ++seq
  latest.set(slot, n)
  return () => latest.get(slot) === n
}

/** `dir` joined with `name` (`''` is the root). */
export function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name
}

/** The directory part of a worktree-relative path (`''` at the root). */
export function parentDir(path: string): string {
  const i = path.lastIndexOf('/')
  return i < 0 ? '' : path.slice(0, i)
}

export function expandedDirs(hostId: string, worktree: string): string[] {
  return usePrefs.getState().expandedDirs[wsKey(hostId, worktree)] ?? []
}

function setExpanded(key: string, dirs: string[]): void {
  const { [key]: _, ...rest } = usePrefs.getState().expandedDirs
  setPrefs({ expandedDirs: dirs.length ? { ...rest, [key]: dirs } : rest })
}

function setDir(key: string, dir: string, state: Loadable<DirListing>): void {
  useFiles.setState((s) => ({ dirs: { ...s.dirs, [key]: { ...s.dirs[key], [dir]: state } } }))
}

/** Load one directory level unless cached (`force`: reload, keeping the old listing meanwhile). */
export async function loadDir(
  hostId: string,
  worktree: string,
  dir: string,
  force = false
): Promise<void> {
  const key = wsKey(hostId, worktree)
  const cur = useFiles.getState().dirs[key]?.[dir]
  if (cur && !force && cur.status !== 'error') return
  if (cur?.status !== 'ok') setDir(key, dir, { status: 'loading' })
  const current = begin(`dir\0${key}\0${dir}`)
  try {
    const listing = await b().listFiles(hostId, worktree, dir, usePrefs.getState().showIgnoredFiles)
    if (current()) setDir(key, dir, { status: 'ok', value: listing })
  } catch (err) {
    if (!current()) return
    const e = failed(err)
    setDir(key, dir, e)
    // Deleted (or now outside the worktree): stop expanding it.
    if (dir && (e.code === 'notFound' || e.code === 'invalidArgument')) {
      setExpanded(
        key,
        expandedDirs(hostId, worktree).filter((d) => d !== dir && !d.startsWith(`${dir}/`))
      )
    }
  }
}

export function toggleDir(hostId: string, worktree: string, dir: string): void {
  const key = wsKey(hostId, worktree)
  const open = expandedDirs(hostId, worktree)
  if (open.includes(dir)) {
    setExpanded(
      key,
      open.filter((d) => d !== dir)
    )
  } else {
    setExpanded(key, [...open, dir])
    void loadDir(hostId, worktree, dir)
  }
}

export function collapseAll(hostId: string, worktree: string): void {
  setExpanded(wsKey(hostId, worktree), [])
}

/** Expand every ancestor of `path` (e.g. after picking it in quick-open). */
export function revealPath(hostId: string, worktree: string, path: string): void {
  const open = new Set(expandedDirs(hostId, worktree))
  for (let dir = parentDir(path); dir; dir = parentDir(dir)) {
    if (!open.has(dir)) {
      open.add(dir)
      void loadDir(hostId, worktree, dir)
    }
  }
  setExpanded(wsKey(hostId, worktree), [...open])
}

/**
 * Reload the root and every expanded directory whose parent is shown, and
 * the quick-open list if it was loaded.
 */
export async function refreshFiles(hostId: string, worktree: string): Promise<void> {
  const key = wsKey(hostId, worktree)
  const open = new Set(expandedDirs(hostId, worktree))
  const visible = [...open].filter((d) => {
    for (let p = parentDir(d); p; p = parentDir(p)) if (!open.has(p)) return false
    return true
  })
  // Hidden listings (under a collapsed parent) are refetched when shown.
  useFiles.setState((s) => {
    const dirs = s.dirs[key] ?? {}
    const kept = Object.fromEntries(
      Object.entries(dirs).filter(([d]) => d === '' || visible.includes(d))
    )
    return { dirs: { ...s.dirs, [key]: kept } }
  })
  const jobs = ['', ...visible].map((d) => loadDir(hostId, worktree, d, true))
  if (useFiles.getState().all[key]) jobs.push(loadAllFiles(hostId, worktree, true))
  await Promise.all(jobs)
}

/** Show or hide gitignored files; cached listings are dropped. */
export function setShowIgnored(show: boolean, active: { hostId: string; worktree: string } | null) {
  setPrefs({ showIgnoredFiles: show })
  useFiles.setState({ dirs: {} })
  if (active) void refreshFiles(active.hostId, active.worktree)
}

/** The flat file list for quick-open, unless cached. */
export async function loadAllFiles(hostId: string, worktree: string, force = false): Promise<void> {
  const key = wsKey(hostId, worktree)
  const cur = useFiles.getState().all[key]
  if (cur && !force && cur.status !== 'error') return
  const set = (v: Loadable<FileList>) => useFiles.setState((s) => ({ all: { ...s.all, [key]: v } }))
  if (cur?.status !== 'ok') set({ status: 'loading' })
  const current = begin(`all\0${key}`)
  try {
    const list = await b().listAllFiles(hostId, worktree)
    if (current()) set({ status: 'ok', value: list })
  } catch (err) {
    if (current()) set(failed(err))
  }
}

export const fileKey = (hostId: string, worktree: string, path: string) =>
  `${wsKey(hostId, worktree)}\u0000${path}`

/** Read a file for its tab (`force`: reload, keeping the old contents meanwhile). */
export async function loadPreview(
  hostId: string,
  worktree: string,
  path: string,
  force = false
): Promise<void> {
  const key = fileKey(hostId, worktree, path)
  const cur = useFiles.getState().previews[key]
  if (cur && !force && cur.status !== 'error') return
  const set = (v: Loadable<FilePreview>) =>
    useFiles.setState((s) => ({ previews: { ...s.previews, [key]: v } }))
  if (cur?.status !== 'ok') set({ status: 'loading' })
  const current = begin(`file\0${key}`)
  try {
    const file = await b().readFile(hostId, worktree, path, PREVIEW_BYTES)
    if (current()) set({ status: 'ok', value: file })
  } catch (err) {
    if (current()) set(failed(err))
  }
}
