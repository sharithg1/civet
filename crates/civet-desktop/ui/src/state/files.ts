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
import type { DirListing, FileEntry, FileList, FilePreview } from '@/types'
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
  /** `fileKey`s of Markdown tabs showing their source instead of the preview */
  showSource: Record<string, true>
  /** `fileKey` → heading to scroll to once that Markdown tab renders (`other.md#x` links) */
  pendingAnchor: Record<string, string>
}

export const useFiles = create<FilesState>(() => ({
  dirs: {},
  all: {},
  previews: {},
  showSource: {},
  pendingAnchor: {}
}))

/** Scroll the Markdown tab of `key` to heading `id` when it next renders (null: done). */
export function setPendingAnchor(key: string, id: string | null): void {
  useFiles.setState(({ pendingAnchor: { [key]: _, ...rest } }) => ({
    pendingAnchor: id ? { ...rest, [key]: id } : rest
  }))
}

/** Switch a Markdown tab between its rendered preview and its source. */
export function toggleSource(key: string): void {
  useFiles.setState(({ showSource: { [key]: on, ...rest } }) => ({
    showSource: on ? rest : { ...rest, [key]: true }
  }))
}

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

/** Expanded directories whose ancestors are all expanded too (shown in the tree). */
function visibleExpanded(open: readonly string[]): string[] {
  const set = new Set(open)
  return open.filter((d) => {
    for (let p = parentDir(d); p; p = parentDir(p)) if (!set.has(p)) return false
    return true
  })
}

/** Load every shown directory not cached yet (e.g. a remembered subfolder of one just expanded). */
function loadShown(hostId: string, worktree: string): void {
  for (const d of visibleExpanded(expandedDirs(hostId, worktree))) void loadDir(hostId, worktree, d)
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
    loadShown(hostId, worktree)
  }
}

export function collapseAll(hostId: string, worktree: string): void {
  setExpanded(wsKey(hostId, worktree), [])
}

/** Expand every ancestor of `path` (e.g. after picking it in quick-open). */
export function revealPath(hostId: string, worktree: string, path: string): void {
  expandDir(hostId, worktree, parentDir(path))
}

/** Expand `dir` and its ancestors, loading what is needed. */
export function expandDir(hostId: string, worktree: string, dir: string): void {
  const open = new Set(expandedDirs(hostId, worktree))
  for (let d = dir; d; d = parentDir(d)) open.add(d)
  setExpanded(wsKey(hostId, worktree), [...open])
  loadShown(hostId, worktree)
}

/**
 * Reload the root and every expanded directory whose parent is shown, and
 * the quick-open list if it was loaded.
 */
export async function refreshFiles(hostId: string, worktree: string): Promise<void> {
  const key = wsKey(hostId, worktree)
  const visible = visibleExpanded(expandedDirs(hostId, worktree))
  // Hidden listings (under a collapsed parent) are refetched when shown
  // (`loadShown`).
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

/**
 * Reload these directories now if they are cached (after a file op), and
 * the quick-open list if it was loaded. Listings of `gone` directories and
 * everything under them are dropped.
 */
export async function refreshDirs(
  hostId: string,
  worktree: string,
  dirs: string[],
  gone: string[] = []
): Promise<void> {
  const key = wsKey(hostId, worktree)
  if (gone.length) {
    useFiles.setState((s) => {
      const cached = s.dirs[key] ?? {}
      const kept = Object.fromEntries(
        Object.entries(cached).filter(([d]) => !gone.some((g) => isUnder(d, g)))
      )
      return { dirs: { ...s.dirs, [key]: kept } }
    })
  }
  const cached = useFiles.getState().dirs[key] ?? {}
  const jobs = [...new Set(dirs)]
    .filter((d) => cached[d])
    .map((d) => loadDir(hostId, worktree, d, true))
  if (useFiles.getState().all[key]) jobs.push(loadAllFiles(hostId, worktree, true))
  await Promise.all(jobs)
}

/** `path` is `root` or inside it. */
export function isUnder(path: string, root: string): boolean {
  return root === '' || path === root || path.startsWith(`${root}/`)
}

/** Expanded directories follow a rename / move of `from` to `to`, or are forgotten (`to` null). */
export function moveExpanded(hostId: string, worktree: string, from: string, to: string | null) {
  const open = expandedDirs(hostId, worktree)
  if (!open.some((d) => isUnder(d, from))) return
  setExpanded(
    wsKey(hostId, worktree),
    open.flatMap((d) => (!isUnder(d, from) ? [d] : to == null ? [] : [to + d.slice(from.length)]))
  )
}

export type TreeRow = { path: string; entry: FileEntry; depth: number }

/** The tree's rows in display order: entries of loaded, expanded directories. */
export function visibleRows(
  dirs: Record<string, Loadable<DirListing>> | undefined,
  expanded: readonly string[]
): TreeRow[] {
  const open = new Set(expanded)
  const out: TreeRow[] = []
  const walk = (dir: string, depth: number) => {
    const l = dirs?.[dir]
    if (l?.status !== 'ok') return
    for (const entry of l.value.entries) {
      const path = joinPath(dir, entry.name)
      out.push({ path, entry, depth })
      if (entry.kind === 'dir' && open.has(path)) walk(path, depth + 1)
    }
  }
  walk('', 0)
  return out
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
