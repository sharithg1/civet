// The ⌘K worktree switcher's model: which rows to show, in what order and
// under which headers. Pure, so the ranking rules are testable; the dialog
// (components/WorktreeSwitcher.tsx) only renders it.
//
// Empty query: the current worktree, then recently visited ones (MRU), then
// the rest in sidebar order. A handful of worktrees is one flat list; more
// get a "Recent" section followed by one section per project.
// Query: fuzzy match on worktree name, task, project and directory name.
// Exact names rank first, recency breaks near-ties, and rows are grouped by
// project only when several projects each contribute more than one row.

import { basename } from './utils'
import { matchFields, splitBranch } from './worktree-view'

export type SwitcherEntry = {
  /** wsKey(hostId, worktree id) */
  key: string
  hostId: string
  /** Repo id, for grouping (names can repeat across hosts). */
  repo: string
  /** Worktree name: the branch (or the directory for a detached HEAD). */
  name: string
  path: string
  project: string
  /** Host label ("This Mac", an ssh destination). */
  host: string
  local: boolean
  task: string | null
}

export type SwitcherRow<T extends SwitcherEntry = SwitcherEntry> = T & {
  current: boolean
  score: number
  /** Matched character indexes in `name`, `project` and `task`. */
  hits: { name: number[]; project: number[]; task: number[] }
}

export type SwitcherGroup<T extends SwitcherEntry = SwitcherEntry> = {
  /** null: no header (flat list). */
  label: string | null
  /** Host label next to a project header. */
  host: string | null
  /** A project section (its rows can leave the project out). */
  project: boolean
  rows: SwitcherRow<T>[]
}

/** Up to this many worktrees, the empty-query view is one flat list. */
export const FEW_WORKTREES = 8

const NO_HITS = { name: [], project: [], task: [] }

export function switcherGroups<T extends SwitcherEntry>(
  entries: readonly T[],
  opts: { query: string; mru: readonly string[]; currentKey: string | null; recentCount: number }
): SwitcherGroup<T>[] {
  const { mru, currentKey } = opts
  const tokens = opts.query.trim().toLowerCase().split(/\s+/).filter(Boolean)
  // Current first, then most recently visited; never visited last.
  const rank = (k: string) => {
    if (k === currentKey) return -1
    const i = mru.indexOf(k)
    return i < 0 ? Infinity : i
  }
  const row = (e: T, score = 0, hits: SwitcherRow['hits'] = NO_HITS): SwitcherRow<T> => ({
    ...e,
    current: e.key === currentKey,
    score,
    hits
  })

  if (!tokens.length) {
    const recent = entries
      .filter((e) => rank(e.key) !== Infinity)
      .sort((a, b) => rank(a.key) - rank(b.key))
      .slice(0, opts.recentCount + (currentKey ? 1 : 0))
    const rest = entries.filter((e) => !recent.includes(e))
    if (entries.length <= FEW_WORKTREES) {
      return [{ label: null, host: null, project: false, rows: [...recent, ...rest].map((e) => row(e)) }]
    }
    const groups: SwitcherGroup<T>[] = []
    if (recent.length) groups.push({ label: 'Recent', host: null, project: false, rows: recent.map((e) => row(e)) })
    for (const list of byProject(rest)) {
      groups.push({ label: list[0]!.project, host: list[0]!.host, project: true, rows: list.map((e) => row(e)) })
    }
    return groups
  }

  const q = tokens.join(' ')
  const rows: SwitcherRow<T>[] = []
  for (const e of entries) {
    const dir = basename(e.path)
    const m = matchFields(tokens, [e.name, e.project, e.task ?? '', dir])
    if (!m) continue
    const short = splitBranch(e.name).name.toLowerCase()
    let score = m.score
    if (e.name.toLowerCase() === q || short === q || dir.toLowerCase() === q) score += 20
    else if (short.startsWith(q) || e.name.toLowerCase().startsWith(q)) score += 6
    else if (e.project.toLowerCase() === q) score += 4
    const r = rank(e.key)
    // Recency nudges near-ties; the current worktree gets none (you're there).
    if (r >= 0 && r !== Infinity) score += Math.max(0, 2 - r * 0.25)
    rows.push(row(e, score, { name: m.hits[0]!, project: m.hits[1]!, task: m.hits[2]! }))
  }
  rows.sort((a, b) => b.score - a.score || rank(a.key) - rank(b.key) || 0)
  const projects = byProject(rows)
  if (projects.length > 1 && projects.filter((l) => l.length > 1).length > 1) {
    return projects
      .sort((a, b) => b[0]!.score - a[0]!.score)
      .map((list) => ({ label: list[0]!.project, host: list[0]!.host, project: true, rows: list }))
  }
  return [{ label: null, host: null, project: false, rows }]
}

/** Group by host + repo, keeping first-seen order. */
function byProject<T extends SwitcherEntry>(list: readonly T[]): T[][] {
  const m = new Map<string, T[]>()
  for (const e of list) {
    const k = `${e.hostId}\u0000${e.repo}`
    m.set(k, [...(m.get(k) ?? []), e])
  }
  return [...m.values()]
}
