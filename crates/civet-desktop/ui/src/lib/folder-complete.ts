// Folder path completion for the Add project dialog: which host directory
// to list for what the user typed, and which of its subdirectories match.
import type { HostDir } from '@/types'

/** Oldest civetd with `ListDirs` (civet_proto::LIST_DIRS_MIN_DAEMON_VERSION). */
export const LIST_DIRS_MIN_DAEMON_VERSION = '0.1.18'

/** What to list for a typed path: `dir` on the host, filtered by `prefix`. */
export type FolderQuery = {
  /** Directory to list, as typed (`~/src/`, `/`, `~`). */
  dir: string
  /** The partial last segment. */
  prefix: string
}

/**
 * Splits a typed path at its last `/`: `~/src/ci` lists `~/src/` for `ci`.
 * `~` alone lists the home directory. Null for an empty or relative path
 * (the daemon would resolve a relative one against its own cwd on submit).
 */
export function folderQuery(input: string): FolderQuery | null {
  if (input === '~') return { dir: '~', prefix: '' }
  if (!input.startsWith('/') && !input.startsWith('~/')) return null
  const slash = input.lastIndexOf('/')
  return { dir: input.slice(0, slash + 1), prefix: input.slice(slash + 1) }
}

/**
 * Subdirectories starting with `prefix` (case-insensitive), exact-case
 * matches first; hidden ones only once the prefix starts with a dot.
 */
export function matchFolders(dirs: HostDir[], prefix: string): HostDir[] {
  const lower = prefix.toLowerCase()
  const hidden = prefix.startsWith('.')
  return dirs
    .filter((d) => (hidden || !d.name.startsWith('.')) && d.name.toLowerCase().startsWith(lower))
    .sort((a, b) => Number(!a.name.startsWith(prefix)) - Number(!b.name.startsWith(prefix)))
}

/** The path a picked folder completes to; the trailing `/` lists its children next. */
export function completeFolder(q: FolderQuery, name: string): string {
  const dir = q.dir === '~' ? '~/' : q.dir
  return `${dir}${name}/`
}
