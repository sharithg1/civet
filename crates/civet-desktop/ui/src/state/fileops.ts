// File management in the Files tab: create, rename / move, duplicate, copy
// and cut / paste, delete (to the daemon's trash, with Undo), uploads from
// Finder and downloads (`file_op`, `upload_to_worktree`, `download_file`;
// civetd >= FILE_OPS_MIN_DAEMON_VERSION, the UI hides them before that).
//
// Every op reloads the directories it touched right away, open file tabs
// follow a renamed or moved file and close with a deleted one, and failures
// become toasts. Ops on several entries run one at a time and report all
// failures in one toast.

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import type { FileOp } from '@/types'
import {
  expandDir,
  isUnder,
  joinPath,
  loadDir,
  moveExpanded,
  parentDir,
  refreshDirs,
  useFiles
} from './files'
import {
  closeFileTabs,
  openFileTab,
  pushToast,
  reportError,
  retargetFileTabs,
  useStore,
  wsKey
} from './store'

/** Oldest civetd with `FileOp` / `ReadFileChunk` (civet_proto::FILE_OPS_MIN_DAEMON_VERSION). */
export const FILE_OPS_MIN_DAEMON_VERSION = '0.1.15'

/** Numeric `a.b.c` comparison; anything unparsable counts as 0. */
export function versionAtLeast(version: string, min: string): boolean {
  const parts = (v: string) => v.split(/[.-]/).map((p) => Number.parseInt(p, 10) || 0)
  const a = parts(version)
  const b = parts(min)
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0)
    if (d) return d > 0
  }
  return true
}

/** The host's daemon can change files (else the Files tab stays read-only). */
export function fileOpsSupported(hostId: string): boolean {
  const v = useStore.getState().hosts[hostId]?.daemonVersion
  return !!v && versionAtLeast(v, FILE_OPS_MIN_DAEMON_VERSION)
}

/** Entries cut or copied in the Files tab, waiting for a paste. */
export type FileClip = {
  hostId: string
  worktree: string
  paths: string[]
  mode: 'cut' | 'copy'
}

export const useFileClipboard = create<{ clip: FileClip | null }>(() => ({ clip: null }))

export function setFileClip(clip: FileClip | null): void {
  useFileClipboard.setState({ clip })
}

function b(): CivetBackend {
  const be = useStore.getState().backend
  if (!be) throw new Error('backend not initialised')
  return be
}

const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1)
const plural = (n: number, what: string) => `${n} ${what}${n === 1 ? '' : 's'}`
const label = (paths: string[]) =>
  paths.length === 1 ? baseName(paths[0]!) : plural(paths.length, 'item')

/** Why `name` can't be a file name, or null if it can. */
export function nameError(name: string): string | null {
  if (!name.trim()) return 'A name is required.'
  if (name.includes('/')) return 'Names can’t contain “/”.'
  if (name === '.' || name === '..') return 'That name is reserved.'
  if (name.toLowerCase() === '.git') return '.git is managed by git.'
  return null
}

/**
 * "a.txt" → "a copy.txt", then "a copy 2.txt"… — the first not in `taken`.
 * Directories and dotfiles keep their whole name before " copy".
 */
export function copyName(name: string, taken: ReadonlySet<string>, isDir = false): string {
  const dot = isDir ? -1 : name.lastIndexOf('.')
  const [stem, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, '']
  for (let n = 1; ; n++) {
    const candidate = `${stem} copy${n > 1 ? ` ${n}` : ''}${ext}`
    if (!taken.has(candidate)) return candidate
  }
}

/** Only the top-most of nested paths (moving a folder moves its contents). */
export function topLevel(paths: string[]): string[] {
  const unique = [...new Set(paths)]
  return unique.filter((p) => !unique.some((q) => q !== p && isUnder(p, q)))
}

function op(hostId: string, worktree: string, o: FileOp) {
  return b().fileOp(hostId, worktree, o)
}

/** Names in `dir` per the cached listing (loaded first if needed). */
async function namesIn(hostId: string, worktree: string, dir: string): Promise<Set<string>> {
  const key = wsKey(hostId, worktree)
  if (useFiles.getState().dirs[key]?.[dir]?.status !== 'ok') await loadDir(hostId, worktree, dir)
  const l = useFiles.getState().dirs[key]?.[dir]
  return new Set(l?.status === 'ok' ? l.value.entries.map((e) => e.name) : [])
}

function isDirEntry(hostId: string, worktree: string, path: string): boolean {
  const l = useFiles.getState().dirs[wsKey(hostId, worktree)]?.[parentDir(path)]
  return (
    l?.status === 'ok' && l.value.entries.some((e) => e.name === baseName(path) && e.kind === 'dir')
  )
}

function reportFailures(title: string, failures: { path: string; err: unknown }[]): void {
  if (!failures.length) return
  if (failures.length === 1) return reportError(title, failures[0]!.err)
  pushToast(
    {
      kind: 'error',
      title,
      body: failures.map((f) => `${f.path}: ${CivetError.from(f.err).message}`).join('\n')
    },
    9000
  )
}

/** Create an empty file or folder `name` in `dir`; a new file opens in a tab. The new path, or null. */
export async function createEntry(
  hostId: string,
  worktree: string,
  dir: string,
  name: string,
  kind: 'file' | 'dir'
): Promise<string | null> {
  const path = joinPath(dir, name)
  try {
    await op(hostId, worktree, kind === 'dir' ? { createDir: { path } } : { createFile: { path } })
  } catch (err) {
    reportError(`Could not create ${name}`, err)
    return null
  }
  expandDir(hostId, worktree, dir)
  await refreshDirs(hostId, worktree, [dir])
  if (kind === 'file') openFileTab(hostId, worktree, path)
  return path
}

/** Tabs, expanded folders and cached listings follow `from` → `to`. */
function moved(hostId: string, worktree: string, from: string, to: string): void {
  retargetFileTabs(hostId, worktree, from, to)
  moveExpanded(hostId, worktree, from, to)
}

/** Rename `path` to `name` (same folder). The new path, or null. */
export async function renameEntry(
  hostId: string,
  worktree: string,
  path: string,
  name: string
): Promise<string | null> {
  const to = joinPath(parentDir(path), name)
  if (to === path) return path
  try {
    await op(hostId, worktree, { move: { from: path, to } })
  } catch (err) {
    reportError(`Could not rename ${baseName(path)}`, err)
    return null
  }
  moved(hostId, worktree, path, to)
  // Loads the renamed folder's remembered open subfolders.
  expandDir(hostId, worktree, parentDir(path))
  await refreshDirs(hostId, worktree, [parentDir(path)], [path])
  return to
}

/** Move entries into `dest` (already there: skipped). The new paths. */
export async function moveEntries(
  hostId: string,
  worktree: string,
  paths: string[],
  dest: string
): Promise<string[]> {
  const todo = topLevel(paths).filter((p) => parentDir(p) !== dest)
  const done: string[] = []
  const failures: { path: string; err: unknown }[] = []
  for (const from of todo) {
    const to = joinPath(dest, baseName(from))
    try {
      if (isUnder(dest, from))
        throw new CivetError('invalidArgument', 'a folder cannot go inside itself')
      await op(hostId, worktree, { move: { from, to } })
      moved(hostId, worktree, from, to)
      done.push(to)
    } catch (err) {
      failures.push({ path: from, err })
    }
  }
  if (done.length) expandDir(hostId, worktree, dest)
  await refreshDirs(hostId, worktree, [dest, ...todo.map(parentDir)], todo)
  reportFailures(`Could not move ${label(failures.map((f) => f.path))}`, failures)
  return done
}

/**
 * Copy entries into `dest` under their own names, or "name copy…" when
 * taken (so copying into the same folder duplicates). The new paths.
 */
export async function copyEntries(
  hostId: string,
  worktree: string,
  paths: string[],
  dest: string
): Promise<string[]> {
  const taken = await namesIn(hostId, worktree, dest)
  const done: string[] = []
  const failures: { path: string; err: unknown }[] = []
  for (const from of topLevel(paths)) {
    const name = baseName(from)
    const isDir = isDirEntry(hostId, worktree, from)
    let target = taken.has(name) ? copyName(name, taken, isDir) : name
    try {
      // Someone else may have taken the name since the listing: try the next.
      for (let attempt = 0; ; attempt++) {
        try {
          await op(hostId, worktree, { copy: { from, to: joinPath(dest, target) } })
          break
        } catch (err) {
          if (CivetError.from(err).code !== 'alreadyExists' || attempt >= 5) throw err
          taken.add(target)
          target = copyName(name, taken, isDir)
        }
      }
      taken.add(target)
      done.push(joinPath(dest, target))
    } catch (err) {
      failures.push({ path: from, err })
    }
  }
  if (done.length) expandDir(hostId, worktree, dest)
  await refreshDirs(hostId, worktree, [dest])
  reportFailures(`Could not copy ${label(failures.map((f) => f.path))}`, failures)
  return done
}

/** Duplicate an entry next to itself ("name copy"). The new path, or null. */
export async function duplicateEntry(
  hostId: string,
  worktree: string,
  path: string
): Promise<string | null> {
  return (await copyEntries(hostId, worktree, [path], parentDir(path)))[0] ?? null
}

/** Undo for a delete: restore toast TTL (the trash itself keeps entries for days). */
const UNDO_MS = 10_000

/**
 * Move entries to the daemon's trash. Their tabs close; a toast offers
 * Undo, which puts them back.
 */
export async function trashEntries(
  hostId: string,
  worktree: string,
  paths: string[]
): Promise<void> {
  const todo = topLevel(paths)
  const trashed: { path: string; trashId: string }[] = []
  const failures: { path: string; err: unknown }[] = []
  for (const path of todo) {
    try {
      const r = await op(hostId, worktree, { trash: { path } })
      if (r.trashId) trashed.push({ path, trashId: r.trashId })
    } catch (err) {
      failures.push({ path, err })
    }
  }
  const gone = trashed.map((t) => t.path)
  closeFileTabs(hostId, worktree, gone)
  for (const p of gone) moveExpanded(hostId, worktree, p, null)
  // Cut entries that are now gone can't be pasted.
  const clip = useFileClipboard.getState().clip
  if (clip && clip.hostId === hostId && clip.worktree === worktree) {
    const left = clip.paths.filter((p) => !gone.some((g) => isUnder(p, g)))
    setFileClip(left.length ? { ...clip, paths: left } : null)
  }
  await refreshDirs(hostId, worktree, gone.map(parentDir), gone)
  if (trashed.length) {
    pushToast(
      {
        kind: 'info',
        title: `Deleted ${label(gone)}`,
        action: { label: 'Undo', run: () => void restoreEntries(hostId, worktree, trashed) }
      },
      UNDO_MS
    )
  }
  reportFailures(`Could not delete ${label(failures.map((f) => f.path))}`, failures)
}

/** Put trashed entries back (newest first). */
export async function restoreEntries(
  hostId: string,
  worktree: string,
  trashed: { path: string; trashId: string }[]
): Promise<void> {
  const failures: { path: string; err: unknown }[] = []
  for (const t of [...trashed].reverse()) {
    try {
      await op(hostId, worktree, { restore: { trashId: t.trashId } })
    } catch (err) {
      failures.push({ path: t.path, err })
    }
  }
  await refreshDirs(
    hostId,
    worktree,
    trashed.map((t) => parentDir(t.path))
  )
  reportFailures(`Could not restore ${label(failures.map((f) => f.path))}`, failures)
}

/** Paste the clipboard into `dest`: a cut moves (once), a copy copies. The new paths. */
export async function pasteEntries(
  hostId: string,
  worktree: string,
  dest: string
): Promise<string[]> {
  const clip = useFileClipboard.getState().clip
  if (!clip) return []
  if (clip.hostId !== hostId || clip.worktree !== worktree) {
    pushToast({ kind: 'info', title: 'Paste works within one worktree' }, 4000)
    return []
  }
  if (clip.mode === 'copy') return copyEntries(hostId, worktree, clip.paths, dest)
  setFileClip(null)
  return moveEntries(hostId, worktree, clip.paths, dest)
}

/** Copy files / folders dropped from Finder into `dir`. */
export async function uploadInto(
  hostId: string,
  worktree: string,
  dir: string,
  localPaths: string[]
): Promise<void> {
  let results
  try {
    results = await b().uploadToWorktree(hostId, worktree, dir, localPaths)
  } catch (err) {
    reportError('Could not upload', err)
    return
  }
  const ok = results.filter((r) => r.path)
  const failed = results.filter((r) => !r.path)
  if (ok.length) expandDir(hostId, worktree, dir)
  await refreshDirs(hostId, worktree, [dir])
  if (ok.length) {
    pushToast(
      { kind: 'info', title: `Uploaded ${label(ok.map((r) => r.path!))} to ${dir || 'the root'}` },
      4000
    )
  }
  if (failed.length) {
    pushToast(
      {
        kind: 'error',
        title: `Could not upload ${label(failed.map((f) => f.source))}`,
        body: failed.map((f) => f.error ?? f.source).join('\n')
      },
      9000
    )
  }
}

/** Save a worktree file on this Mac (a save dialog asks where). */
export async function downloadEntry(hostId: string, worktree: string, path: string): Promise<void> {
  try {
    const dest = await b().downloadFile(hostId, worktree, path)
    if (dest) pushToast({ kind: 'info', title: `Downloaded ${baseName(path)} to ${dest}` }, 4000)
  } catch (err) {
    reportError(`Could not download ${baseName(path)}`, err)
  }
}
