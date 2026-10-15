// Client-side preferences (Settings → General), persisted in localStorage.
// Only settings the UI can honour without backend support live here; the
// rest of the v3 Settings design is listed as a backend follow-up.

import { create } from 'zustand'

export type WorktreeLabel = 'task' | 'branch'
export type SidebarTabs = 'selected' | 'all' | 'none'
export type StepKind = 'agent' | 'terminal'
export type CreateStep = { kind: StepKind; cmd: string }
/** Local echo prediction: `auto` = mosh-style, only on links slower than ~30 ms. */
export type Prediction = 'auto' | 'off'

type PrefsListener = (next: Prefs, prev: Prefs) => void
const listeners = new Set<PrefsListener>()

/** Called after every `setPrefs` / `resetPrefs` (e.g. to push `set_prediction`). */
export function onPrefsChange(cb: PrefsListener): () => void {
  listeners.add(cb)
  return () => listeners.delete(cb)
}

export type Prefs = {
  /** Primary worktree label: the agent's task title (pane title) or the branch. */
  worktreeLabel: WorktreeLabel
  /** Which sidebar worktrees list their tabs underneath. */
  sidebarTabs: SidebarTabs
  /** Number of "Recent" entries in the worktree switcher. */
  recentCount: number
  /** Command for new Claude tabs (⌥C) and the default on-create step. */
  agentCommand: string
  /** Ask before closing a pane whose agent is still working. */
  confirmCloseRunning: boolean
  /** Saved on-create steps per `${hostId}\0${repoId}` ("Save as default for …"). */
  createSteps: Record<string, CreateStep[]>
  /** Local echo prediction (sent to the core with `set_prediction`). */
  prediction: Prediction
  /** Worktrees hidden from the sidebar's main list, by `wsKey(hostId, worktree)`. */
  archivedWorktrees: Record<string, true>
  /** Sidebar layout (drag to reorder): host ids; hosts not listed follow. */
  hostOrder: string[]
  /** Sidebar layout: repo ids per host; repos not listed follow. */
  repoOrder: Record<string, string[]>
  /** Collapsed hosts in the sidebar. */
  collapsedHosts: Record<string, true>
  /** Collapsed projects in the sidebar, by `wtKey(hostId, repoId)`. */
  collapsedRepos: Record<string, true>
  /** Panels as last toggled (⌥B / ⌥E); null: the default for the window size. */
  sidebarOpen: boolean | null
  rightPanelOpen: boolean | null
  /** Right panel tab last selected. */
  rightTab: 'files' | 'changes' | 'branch'
}

/** Fields kept by "Reset to defaults": saved data and sidebar layout, not settings. */
const KEPT_ON_RESET = [
  'createSteps',
  'archivedWorktrees',
  'hostOrder',
  'repoOrder',
  'collapsedHosts',
  'collapsedRepos',
  'sidebarOpen',
  'rightPanelOpen',
  'rightTab'
] as const

export const DEFAULT_PREFS: Prefs = {
  worktreeLabel: 'task',
  sidebarTabs: 'selected',
  recentCount: 5,
  agentCommand: 'claude',
  confirmCloseRunning: true,
  createSteps: {},
  prediction: 'auto',
  archivedWorktrees: {},
  hostOrder: [],
  repoOrder: {},
  collapsedHosts: {},
  collapsedRepos: {},
  sidebarOpen: null,
  rightPanelOpen: null,
  rightTab: 'branch'
}

const KEY = 'civet.prefs.v1'

function load(): Prefs {
  try {
    const raw = typeof localStorage !== 'undefined' ? localStorage.getItem(KEY) : null
    if (!raw) return DEFAULT_PREFS
    return { ...DEFAULT_PREFS, ...(JSON.parse(raw) as Partial<Prefs>) }
  } catch {
    return DEFAULT_PREFS
  }
}

export const usePrefs = create<Prefs>(load)

export function setPrefs(patch: Partial<Prefs>): void {
  const prev = usePrefs.getState()
  usePrefs.setState(patch)
  const next = usePrefs.getState()
  try {
    localStorage.setItem(KEY, JSON.stringify(next))
  } catch {
    // Storage unavailable (private mode): keep the in-memory value.
  }
  for (const cb of listeners) cb(next, prev)
}

export function resetPrefs(): void {
  const cur = usePrefs.getState()
  const kept = Object.fromEntries(KEPT_ON_RESET.map((k) => [k, cur[k]])) as Partial<Prefs>
  setPrefs({ ...DEFAULT_PREFS, ...kept })
}

/** `ids` in the saved `order` first, then the rest in their original order. */
export function applyOrder(ids: readonly string[], order: readonly string[] | undefined): string[] {
  if (!order?.length) return [...ids]
  const rank = new Map(order.map((id, i) => [id, i]))
  const known = ids.filter((id) => rank.has(id)).sort((a, b) => rank.get(a)! - rank.get(b)!)
  return [...known, ...ids.filter((id) => !rank.has(id))]
}
