// App state (zustand, like Orca's renderer store, but a single small slice).
// Hosts/repos/worktrees/panes mirror the daemon; workspaces hold the UI-only
// tab + split layout per (host, worktree).

import { create } from 'zustand'
import type { CivetBackend } from '../backend'
import { CivetError } from '../backend'
import {
  leaf,
  leaves,
  removeLeaf,
  splitLeaf,
  type NodePath,
  type PaneNode,
  type SplitDirection,
  setRatio
} from '../layout/pane-tree'
import { aggregateStatus } from '../lib/agent-status'
import { promptLabel, worktreeBranch } from '../lib/worktree-view'
import { splitShellWords } from '../lib/shell-words'
import {
  agentCommandFor,
  applyOrder,
  onPrefsChange,
  setPrefs,
  stepAgent,
  usePrefs,
  type AgentKind,
  type CreateStep,
  type Prefs
} from './prefs'
import { forgetHost, loadSavedHosts, savedHost, saveHost } from './saved-hosts'
import type {
  AgentAttentionEvent,
  AgentStatus,
  BootstrapProgressEvent,
  BootstrapStep,
  ClipboardEvent,
  CommandSpec,
  DaemonEvent,
  HostState,
  HostStateEvent,
  HostSummary,
  HostTarget,
  PaneId,
  PaneInfo,
  PullRequestInfo,
  RepoInfo,
  TermSize,
  TransportChoice,
  WorktreeInfo
} from '../types'
import { termSize } from '../types'

export type Tab = { id: string; root: PaneNode; focused: PaneId }
export type Workspace = { tabs: Tab[]; activeTab: string | null }

export type HostView = HostSummary & {
  rttMs: number | null
  message: string | null
  bootstrap: { step: BootstrapStep; message: string | null } | null
  /** Error code of the last failed connect (`daemonBusy` offers a daemon restart). */
  errorCode: string | null
}

export type Toast = {
  id: number
  kind: 'attention' | 'error' | 'info'
  title: string
  body?: string
  status?: 'done' | 'needsInput'
  target?: { hostId: string; worktree: string | null; pane: PaneId }
}

export type ConfirmRequest = {
  title: string
  body: string
  confirmLabel: string
  onConfirm: () => void
  /** Focus Cancel instead of the (destructive) confirm button. */
  focusCancel?: boolean
}

export type NewWorktreeRequest = { hostId: string; repo?: string }

export type RightTab = Prefs['rightTab']
export type SettingsPage = 'general' | 'hosts' | 'projects' | 'keyboard'

type State = {
  backend: CivetBackend | null
  hosts: Record<string, HostView>
  hostOrder: string[]
  repos: Record<string, RepoInfo[]>
  /** key: wtKey(hostId, repoId) */
  worktrees: Record<string, WorktreeInfo[]>
  /** key: wtKey(hostId, repoId) → head branch → its newest PR */
  pullRequests: Record<string, Record<string, PullRequestInfo>>
  panes: Record<string, Record<PaneId, PaneInfo>>
  /** key: wsKey(hostId, worktreeId) */
  workspaces: Record<string, Workspace>
  active: { hostId: string; worktree: string } | null
  /** key: wsKey; set when an agent wants attention outside the active worktree */
  unread: Record<string, AgentStatus>
  showConnect: boolean
  newWorktree: NewWorktreeRequest | null
  /** "Add project" dialog: open, with the location preselected (null: default). */
  addProject: { hostId: string | null } | null
  /** Reopen "Add project" on the host a pending "New SSH host…" connects. */
  addProjectAfterConnect: boolean
  confirm: ConfirmRequest | null
  toasts: Toast[]
  // ── v3 chrome (UI-only) ──
  sidebarOpen: boolean
  rightPanelOpen: boolean
  rightTab: RightTab
  switcherOpen: boolean
  settings: SettingsPage | null
  removeWorktree: { hostId: string; worktree: string } | null
  /** wsKeys of worktrees whose removal is in flight (see removeWorktree) */
  removing: Record<string, true>
  /** wsKey → git's refusal of the last removal; the dialog then offers "Remove anyway" */
  removeRefused: Record<string, string>
  /** key: wtKey(hostId, repoId) of collapsed project groups */
  /** wsKeys, most recently selected first */
  mru: string[]
  /** wsKey → when it was last left / selected (ms) */
  seen: Record<string, number>
  /** hostId → "running>available" upgrade the user postponed ("Later") */
  dismissedUpgrades: Record<string, string>
}

export const useStore = create<State>(() => ({
  backend: null,
  hosts: {},
  hostOrder: [],
  repos: {},
  worktrees: {},
  pullRequests: {},
  panes: {},
  workspaces: {},
  active: null,
  unread: {},
  showConnect: false,
  newWorktree: null,
  addProject: null,
  addProjectAfterConnect: false,
  confirm: null,
  toasts: [],
  // Panels start as last left (prefs), else open (right: on wide windows).
  sidebarOpen: usePrefs.getState().sidebarOpen ?? true,
  rightPanelOpen:
    usePrefs.getState().rightPanelOpen ??
    (typeof window === 'undefined' || window.innerWidth >= 1100),
  rightTab: usePrefs.getState().rightTab,
  switcherOpen: false,
  settings: null,
  removeWorktree: null,
  removing: {},
  removeRefused: {},
  mru: [],
  seen: {},
  dismissedUpgrades: {}
}))

// Remember the panel toggles (title bar buttons, ⌥B / ⌥E) and the right tab.
useStore.subscribe((s, prev) => {
  if (
    s.sidebarOpen !== prev.sidebarOpen ||
    s.rightPanelOpen !== prev.rightPanelOpen ||
    s.rightTab !== prev.rightTab
  ) {
    setPrefs({ sidebarOpen: s.sidebarOpen, rightPanelOpen: s.rightPanelOpen, rightTab: s.rightTab })
  }
})

const set = useStore.setState
const get = useStore.getState

export const wtKey = (hostId: string, repo: string) => `${hostId}\u0000${repo}`
export const wsKey = (hostId: string, worktree: string) => `${hostId}\u0000${worktree}`

/** True while `removeWorktree` runs for this worktree: no new tabs or panes there. */
export const isRemoving = (hostId: string, worktree: string) =>
  !!get().removing[wsKey(hostId, worktree)]

/** Size used for spawns before the pane is measured; the pane resizes on mount. */
export const DEFAULT_SPAWN_SIZE: TermSize = termSize(120, 32)

function backend(): CivetBackend {
  const b = get().backend
  if (!b) throw new Error('backend not initialised')
  return b
}

let toastSeq = 1
export function pushToast(t: Omit<Toast, 'id'>, ttlMs = 6000): void {
  const id = toastSeq++
  set((s) => ({ toasts: [...s.toasts, { ...t, id }].slice(-5) }))
  if (ttlMs > 0) setTimeout(() => dismissToast(id), ttlMs)
}
export function dismissToast(id: number): void {
  set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }))
}

export function reportError(title: string, err: unknown): void {
  const e = CivetError.from(err)
  console.error(`[civet] ${title}`, e)
  pushToast({ kind: 'error', title, body: e.message }, 9000)
}

// ── Initialisation ────────────────────────────────────────────────────────

export async function initApp(b: CivetBackend): Promise<() => void> {
  set({ backend: b })
  const unlisten = await Promise.all([
    b.onHostState(onHostState),
    b.onBootstrapProgress(onBootstrapProgress),
    b.onDaemonEvent(({ hostId, event }) => onDaemonEvent(hostId, event)),
    b.onAgentAttention(onAgentAttention),
    b.onClipboard(onClipboard)
  ])
  const nudge = () => void b.notifyNetworkChange().catch(() => {})
  const onVisibility = () => {
    if (document.visibilityState !== 'visible') return
    nudge()
    resyncWorktrees()
  }
  window.addEventListener('online', nudge)
  document.addEventListener('visibilitychange', onVisibility)
  const prTimer = setInterval(resyncPullRequests, PR_REFRESH_MS)

  // Local echo prediction preference (Settings → General); Rust defaults to on.
  const pushPrediction = () =>
    void b
      .setPrediction(usePrefs.getState().prediction !== 'off')
      .catch((err: unknown) => reportError('Could not change local echo prediction', err))
  pushPrediction()
  const unPrefs = onPrefsChange((next, prev) => {
    if (next.prediction !== prev.prediction) pushPrediction()
  })

  try {
    const hosts = await b.listHosts()
    for (const h of hosts) upsertHost(h)
    await Promise.all(
      hosts.filter((h) => h.state === 'connected').map((h) => refreshHost(h.hostId))
    )
    // Fresh launch: reconnect the hosts saved last time instead of asking again.
    // runConnect registers each host right away, so the workspace (not the
    // connect screen) shows while they connect.
    const saved = hosts.length === 0 ? loadSavedHosts() : []
    for (const h of saved) {
      void connectHost(h.target, h.transport).catch((err) =>
        reportError(`Could not reconnect to ${h.hostId}`, err)
      )
    }
    set({ showConnect: hosts.length === 0 && saved.length === 0 })
  } catch (err) {
    reportError('Could not list hosts', err)
    set({ showConnect: true })
  }

  return () => {
    unPrefs()
    clearInterval(prTimer)
    unlisten.forEach((u) => u())
    window.removeEventListener('online', nudge)
    document.removeEventListener('visibilitychange', onVisibility)
  }
}

function upsertHost(h: HostSummary, extra: Partial<HostView> = {}): void {
  set((s) => {
    const prev = s.hosts[h.hostId]
    return {
      hosts: {
        ...s.hosts,
        [h.hostId]: {
          rttMs: null,
          message: null,
          bootstrap: null,
          errorCode: null,
          ...prev,
          ...h,
          ...extra
        }
      },
      hostOrder: s.hostOrder.includes(h.hostId) ? s.hostOrder : [...s.hostOrder, h.hostId]
    }
  })
}

// ── Event handlers ────────────────────────────────────────────────────────

export function onHostState(e: HostStateEvent): void {
  const prev = get().hosts[e.hostId]
  const prevState: HostState | undefined = prev?.state
  const upgrade = e.upgrade ?? null
  upsertHost(
    {
      hostId: e.hostId,
      label: prev?.label ?? e.hostId,
      state: e.state,
      transport: e.transport,
      daemonVersion: upgrade?.runningVersion ?? prev?.daemonVersion ?? null,
      instanceId: prev?.instanceId ?? null,
      upgrade
    },
    {
      rttMs: e.rttMs,
      message: e.message,
      bootstrap: e.state === 'connected' ? null : (prev?.bootstrap ?? null),
      errorCode: e.state === 'connected' ? null : (prev?.errorCode ?? null)
    }
  )
  if (upgrade) announceUpgrade(e.hostId)
  // After a re-bootstrap the daemon may be a new instance with different
  // panes; re-sync whenever we come back to connected.
  if (e.state === 'connected' && prevState && prevState !== 'connected') {
    void refreshHost(e.hostId)
    if (prevState === 'reconnecting') {
      pushToast(
        { kind: 'info', title: `Reconnected to ${prev?.label ?? e.hostId} · sessions resumed` },
        3200
      )
    }
  }
}

export function onBootstrapProgress(e: BootstrapProgressEvent): void {
  const prev = get().hosts[e.hostId]
  if (!prev) return
  upsertHost(prev, { bootstrap: { step: e.step, message: e.message } })
}

/**
 * Add a pane from a command reply (spawn / launch). An entry that already
 * exists came from daemon events (paneCreated, agentStatus…) that are at least
 * as new as the reply's snapshot, so it is kept.
 */
export function addPaneSnapshot(hostId: string, pane: PaneInfo): void {
  set((s) => {
    const panes = s.panes[hostId] ?? {}
    if (panes[pane.id]) return {}
    return { panes: { ...s.panes, [hostId]: { ...panes, [pane.id]: pane } } }
  })
}

function patchPane(hostId: string, pane: PaneId, patch: Partial<PaneInfo>): void {
  set((s) => {
    const panes = s.panes[hostId]
    const cur = panes?.[pane]
    if (!panes || !cur) return {}
    return { panes: { ...s.panes, [hostId]: { ...panes, [pane]: { ...cur, ...patch } } } }
  })
}

export function onDaemonEvent(hostId: string, event: DaemonEvent): void {
  if (event === 'reposChanged') {
    void refreshRepos(hostId)
    return
  }
  if ('paneCreated' in event) {
    const p = event.paneCreated
    set((s) => ({ panes: { ...s.panes, [hostId]: { ...s.panes[hostId], [p.id]: p } } }))
  } else if ('paneExited' in event) {
    patchPane(hostId, event.paneExited.pane, { exited: true, exitCode: event.paneExited.exitCode })
  } else if ('paneClosed' in event) {
    removePaneEverywhere(hostId, event.paneClosed.pane)
  } else if ('paneTitle' in event) {
    patchPane(hostId, event.paneTitle.pane, { title: event.paneTitle.title })
  } else if ('paneResized' in event) {
    patchPane(hostId, event.paneResized.pane, { size: event.paneResized.size })
  } else if ('agentStatus' in event) {
    const e = event.agentStatus
    const cur = get().panes[hostId]?.[e.pane]
    // A hook `running` with a message is a prompt submit (civetd's Claude
    // UserPromptSubmit hook): the first one labels the session.
    const prompt =
      e.source === 'hook' && e.status === 'running' && !cur?.prompt ? promptLabel(e.message) : null
    patchPane(hostId, e.pane, prompt ? { status: e.status, prompt } : { status: e.status })
  } else if ('worktreesChanged' in event) {
    void refreshWorktrees(hostId, event.worktreesChanged.repo)
  }
}

/**
 * The Rust side shows the native notification (only when no window is
 * focused, INTERFACES §6.3); the UI adds an in-app toast and an unread badge.
 */
export function onAgentAttention(e: AgentAttentionEvent): void {
  const s = get()
  const isActive =
    s.active?.hostId === e.hostId && e.worktree !== null && s.active.worktree === e.worktree
  if (e.worktree && !(isActive && document.hasFocus())) {
    set((st) => ({ unread: { ...st.unread, [wsKey(e.hostId, e.worktree!)]: e.status } }))
  }
  pushToast({
    kind: 'attention',
    status: e.status,
    title: e.title || `Pane ${e.pane}`,
    body: e.message ?? (e.status === 'done' ? 'Agent finished' : 'Agent needs input'),
    target: { hostId: e.hostId, worktree: e.worktree, pane: e.pane }
  })
}

/**
 * A pane's program copied text (OSC 52); the Rust side already wrote the
 * clipboard. Success is silent (the program shows its own feedback); only a
 * failed write is surfaced.
 */
export function onClipboard(e: ClipboardEvent): void {
  if (!e.ok) {
    pushToast(
      { kind: 'error', title: 'Copy failed', body: `Pane ${e.pane} could not set the clipboard` },
      6000
    )
  }
}

// ── Loading ───────────────────────────────────────────────────────────────

export async function refreshHost(hostId: string): Promise<void> {
  try {
    await Promise.all([refreshRepos(hostId), refreshPanes(hostId)])
  } catch (err) {
    reportError(`Could not load ${hostId}`, err)
  }
}

async function refreshRepos(hostId: string): Promise<void> {
  const repos = await backend().listRepos(hostId)
  set((s) => ({ repos: { ...s.repos, [hostId]: repos } }))
  await Promise.all(repos.map((r) => refreshWorktrees(hostId, r.id)))
}

// ── Worktree list sync ────────────────────────────────────────────────────
// git (via civetd) is the source of truth; the store converges to it:
// * one `list_worktrees` per repo at a time; requests that arrive meanwhile
//   collapse into one trailing re-list (events come in bursts);
// * every create/remove this client applies bumps the repo's epoch, and a
//   list that was requested before the bump is dropped and re-listed. Why:
//   a slow list taken before `git worktree add` finished would otherwise
//   land after the new row and wipe it (and its selection) again.

/** key: wtKey; bumped by every create/remove applied locally. */
const wtEpoch = new Map<string, number>()
const wtRefreshing = new Map<string, { again: boolean; done: Promise<void> }>()

function bumpWorktreeEpoch(hostId: string, repo: string): void {
  const k = wtKey(hostId, repo)
  wtEpoch.set(k, (wtEpoch.get(k) ?? 0) + 1)
}

/** Re-list a repo's worktrees (coalesced, newest wins; see above). */
export function refreshWorktrees(hostId: string, repo: string): Promise<void> {
  const k = wtKey(hostId, repo)
  const running = wtRefreshing.get(k)
  if (running) {
    running.again = true
    return running.done
  }
  const entry = { again: true, done: Promise.resolve() }
  wtRefreshing.set(k, entry)
  entry.done = (async () => {
    try {
      while (entry.again) {
        entry.again = false
        const epoch = wtEpoch.get(k) ?? 0
        const list = await backend().listWorktrees(hostId, repo)
        if ((wtEpoch.get(k) ?? 0) !== epoch) entry.again = true
        else applyWorktreeList(hostId, repo, list)
      }
      refreshPullRequests(hostId, repo)
    } catch (err) {
      reportError(`Could not list worktrees of ${repo}`, err)
    } finally {
      wtRefreshing.delete(k)
    }
  })()
  return entry.done
}

const sameWorktree = (a: WorktreeInfo, b: WorktreeInfo) =>
  a.id === b.id &&
  a.path === b.path &&
  a.branch === b.branch &&
  a.head === b.head &&
  a.isMain === b.isMain &&
  a.locked === b.locked

/**
 * Replace a repo's worktrees with git's list. Worktrees that are gone
 * (removed from a terminal, directory deleted) lose their tabs, and if one
 * was selected the selection moves to the repo's main worktree. Their panes
 * keep running on the host; they reappear if the worktree does.
 */
export function applyWorktreeList(hostId: string, repo: string, list: WorktreeInfo[]): void {
  const k = wtKey(hostId, repo)
  const prev = get().worktrees[k] ?? []
  if (prev.length === list.length && prev.every((w, i) => sameWorktree(w, list[i]!))) return
  const ids = new Set(list.map((w) => w.id))
  const gone = new Set(prev.filter((w) => !ids.has(w.id)).map((w) => wsKey(hostId, w.id)))
  set((s) => {
    const workspaces = { ...s.workspaces }
    const unread = { ...s.unread }
    for (const key of gone) {
      delete workspaces[key]
      delete unread[key]
    }
    return {
      worktrees: { ...s.worktrees, [k]: list },
      workspaces,
      unread,
      mru: s.mru.filter((key) => !gone.has(key))
    }
  })
  const a = get().active
  if (a && gone.has(wsKey(a.hostId, a.worktree))) {
    const main = list.find((w) => w.isMain)
    if (main) selectWorktree(hostId, main.id)
    else set({ active: null })
  }
}

/**
 * A worktree this client just created (create/launch reply): show it at
 * once, then re-list. Upserts by id: the WorktreesChanged event may already
 * have added it.
 */
export function applyCreatedWorktree(hostId: string, wt: WorktreeInfo): void {
  bumpWorktreeEpoch(hostId, wt.repo)
  set((s) => {
    const k = wtKey(hostId, wt.repo)
    const list = s.worktrees[k] ?? []
    return {
      worktrees: { ...s.worktrees, [k]: list.some((w) => w.id === wt.id) ? list : [...list, wt] }
    }
  })
  void refreshWorktrees(hostId, wt.repo)
}

/**
 * Re-list every repo of every connected host. Why: on an older civetd only
 * Civet's own changes produce WorktreesChanged; this catches work done in a
 * terminal when the window comes back.
 */
export function resyncWorktrees(): void {
  const s = get()
  for (const [hostId, h] of Object.entries(s.hosts)) {
    if (h.state !== 'connected') continue
    for (const r of s.repos[hostId] ?? []) void refreshWorktrees(hostId, r.id)
  }
}

// ── Pull requests ─────────────────────────────────────────────────────────
// `gh pr list` on the host (civetd >= 0.1.5), at most once a minute per
// repo: after worktree re-lists and on a timer (initApp). Failures (no gh,
// not logged in, not a GitHub repo, older daemon) leave rows without a PR.

const PR_REFRESH_MS = 60_000
/** key: wtKey → when its last `list_pull_requests` started (ms) */
const prFetched = new Map<string, number>()

export function refreshPullRequests(hostId: string, repo: string): void {
  const k = wtKey(hostId, repo)
  const now = Date.now()
  if (now - (prFetched.get(k) ?? -Infinity) < PR_REFRESH_MS) return
  prFetched.set(k, now)
  backend()
    .listPullRequests(hostId, repo)
    .then(
      (list) => {
        const byBranch: Record<string, PullRequestInfo> = {}
        for (const pr of list) byBranch[pr.branch] ??= pr // newest first
        const prev = get().pullRequests[k]
        if (prev && JSON.stringify(prev) === JSON.stringify(byBranch)) return
        set((s) => ({ pullRequests: { ...s.pullRequests, [k]: byBranch } }))
      },
      (err: unknown) => console.debug(`list_pull_requests ${repo} on ${hostId}:`, err)
    )
}

export async function openPullRequest(pr: PullRequestInfo): Promise<void> {
  try {
    await backend().openPullRequest(pr.url)
  } catch (err) {
    reportError(`Could not open PR #${pr.number}`, err)
  }
}

function resyncPullRequests(): void {
  const s = get()
  for (const [hostId, h] of Object.entries(s.hosts)) {
    if (h.state !== 'connected') continue
    for (const r of s.repos[hostId] ?? []) refreshPullRequests(hostId, r.id)
  }
}

async function refreshPanes(hostId: string): Promise<void> {
  const list = await backend().listPanes(hostId)
  const map: Record<PaneId, PaneInfo> = {}
  const prev = get().panes[hostId] ?? {}
  for (const p of list) {
    const prompt = prev[p.id]?.prompt
    map[p.id] = prompt ? { ...p, prompt } : p
  }
  set((s) => ({ panes: { ...s.panes, [hostId]: map } }))
  // Drop layout leaves for panes that no longer exist (e.g. daemon restarted).
  for (const key of Object.keys(get().workspaces)) {
    if (!key.startsWith(`${hostId}\u0000`)) continue
    for (const tab of get().workspaces[key]!.tabs) {
      for (const pane of leaves(tab.root)) if (!map[pane]) removePaneEverywhere(hostId, pane)
    }
  }
}

// ── Hosts ─────────────────────────────────────────────────────────────────

/** Live panes of a host as the UI knows them (falls back to the bootstrap count). */
export function livePaneCount(hostId: string): number {
  const s = get()
  const panes = s.panes[hostId]
  if (panes) return Object.keys(panes).length
  return s.hosts[hostId]?.upgrade?.livePanes ?? 0
}

/** "civetd v0.1.1 running · v0.1.2 available · 3 live panes" */
export function upgradeSummary(hostId: string): string | null {
  const u = get().hosts[hostId]?.upgrade
  if (!u) return null
  const n = livePaneCount(hostId)
  return `civetd v${u.runningVersion} running · v${u.availableVersion} available · ${n} live pane${n === 1 ? '' : 's'}`
}

const announcedUpgrades = new Set<string>()

/** One toast per host and version pair: the banner and host popover keep the action. */
function announceUpgrade(hostId: string): void {
  const h = get().hosts[hostId]
  const u = h?.upgrade
  if (!h || !u) return
  const key = `${hostId}\u0000${u.runningVersion}\u0000${u.availableVersion}`
  if (announcedUpgrades.has(key)) return
  announcedUpgrades.add(key)
  pushToast(
    {
      kind: 'info',
      title: `civetd v${u.availableVersion} is available on ${h.label}`,
      body: `v${u.runningVersion} keeps running because it has live panes. Restart the daemon when convenient (from the banner or the host menu).`
    },
    9000
  )
}

/** Shared tail of connect / reconnect / restart: track state, adopt the summary, load the host. */
async function runConnect(
  hostId: string,
  label: string,
  connect: () => Promise<HostSummary>
): Promise<HostSummary> {
  const prev = get().hosts[hostId]
  upsertHost(
    prev ?? {
      hostId,
      label,
      state: 'connecting',
      transport: null,
      daemonVersion: null,
      instanceId: null,
      upgrade: null
    },
    { state: 'connecting', message: null, errorCode: null }
  )
  let summary: HostSummary
  try {
    summary = await connect()
  } catch (err) {
    const cur = get().hosts[hostId]
    const e = CivetError.from(err)
    if (cur && cur.state === 'connecting') {
      upsertHost(cur, { state: 'failed', message: e.message, bootstrap: null, errorCode: e.code })
    } else if (cur) {
      upsertHost(cur, { errorCode: e.code })
    }
    throw err
  }
  upsertHost(summary, { bootstrap: null, errorCode: null })
  if (summary.upgrade) announceUpgrade(summary.hostId)
  await refreshHost(summary.hostId)
  return summary
}

export async function connectHost(target: HostTarget, transport: TransportChoice): Promise<void> {
  const hostId = target.kind === 'local' ? 'local' : target.destination.trim()
  const summary = await runConnect(hostId, target.kind === 'local' ? 'This Mac' : hostId, () =>
    backend().connectHost(target, transport)
  )
  saveHost({ hostId: summary.hostId, target, transport })
  set({ showConnect: false })
  if (get().addProjectAfterConnect) {
    set({ addProject: { hostId: summary.hostId }, addProjectAfterConnect: false })
  }
  // Focus the first worktree that has panes, else the first main worktree.
  if (!get().active) {
    const first = worktreesOf(summary.hostId)[0]
    if (first) selectWorktree(summary.hostId, first.id)
  }
}

/**
 * Reconnect a failed / disconnected host with the target and transport it was
 * connected with (ssh port and extra args included; Rust keeps them).
 */
export async function reconnectHost(hostId: string): Promise<void> {
  const label = get().hosts[hostId]?.label ?? hostId
  // A saved host whose launch-time reconnect failed may be unknown to Rust,
  // so reconnect from the saved target.
  const saved = savedHost(hostId)
  try {
    await runConnect(hostId, label, () =>
      saved ? backend().connectHost(saved.target, saved.transport) : backend().reconnectHost(hostId)
    )
  } catch (err) {
    reportError(`Could not reconnect to ${label}`, err)
  }
}

/** Ask, then restart the host's daemon on this app's version (its panes close). */
export function requestRestartDaemon(hostId: string): void {
  const h = get().hosts[hostId]
  if (!h) return
  const n = livePaneCount(hostId)
  const u = h.upgrade
  const what = u
    ? `civetd v${u.runningVersion} on ${h.label} with v${u.availableVersion}`
    : `civetd on ${h.label}`
  set({
    confirm: {
      title: `Restart daemon on ${h.label}?`,
      body:
        `This replaces ${what}. ${n} live pane${n === 1 ? '' : 's'} (agents and shells) will be closed ` +
        'and their processes stop. Worktrees and files are not touched.',
      confirmLabel: `Restart daemon (closes ${n} pane${n === 1 ? '' : 's'})`,
      onConfirm: () => void restartDaemon(hostId),
      focusCancel: true
    }
  })
}

export async function restartDaemon(hostId: string): Promise<void> {
  const label = get().hosts[hostId]?.label ?? hostId
  try {
    const summary = await runConnect(hostId, label, () => backend().restartDaemon(hostId))
    pushToast(
      {
        kind: 'info',
        title: `Restarted civetd on ${label}${summary.daemonVersion ? ` · v${summary.daemonVersion}` : ''}`
      },
      4000
    )
  } catch (err) {
    reportError(`Could not restart the daemon on ${label}`, err)
  }
}

export async function disconnectHost(hostId: string): Promise<void> {
  forgetHost(hostId)
  try {
    await backend().disconnectHost(hostId)
  } catch (err) {
    reportError(`Disconnect ${hostId}`, err)
  }
  set((s) => {
    const hosts = { ...s.hosts }
    delete hosts[hostId]
    const hostOrder = s.hostOrder.filter((h) => h !== hostId)
    return {
      hosts,
      hostOrder,
      active: s.active?.hostId === hostId ? null : s.active,
      showConnect: hostOrder.length === 0
    }
  })
}

export function worktreesOf(hostId: string): WorktreeInfo[] {
  const s = get()
  return (s.repos[hostId] ?? []).flatMap((r) => s.worktrees[wtKey(hostId, r.id)] ?? [])
}

export function panesOfWorktree(
  panes: Record<PaneId, PaneInfo> | undefined,
  worktree: string
): PaneInfo[] {
  return Object.values(panes ?? {}).filter((p) => p.worktree === worktree)
}

export function worktreeStatus(hostId: string, worktree: string): AgentStatus {
  return aggregateStatus(panesOfWorktree(get().panes[hostId], worktree))
}

// ── Workspaces, tabs and splits ───────────────────────────────────────────

let tabSeq = 1
const newTabId = () => `tab-${tabSeq++}`

function updateWorkspace(key: string, fn: (ws: Workspace) => Workspace): void {
  set((s) => ({
    workspaces: { ...s.workspaces, [key]: fn(s.workspaces[key] ?? { tabs: [], activeTab: null }) }
  }))
}

export function selectWorktree(hostId: string, worktree: string): void {
  const key = wsKey(hostId, worktree)
  set((s) => {
    const unread = { ...s.unread }
    delete unread[key]
    const now = Date.now()
    const prev = s.active ? wsKey(s.active.hostId, s.active.worktree) : null
    const seen = { ...s.seen, [key]: now }
    if (prev && prev !== key) seen[prev] = now
    return {
      active: { hostId, worktree },
      unread,
      seen,
      mru: [key, ...s.mru.filter((k) => k !== key)].slice(0, 50),
      switcherOpen: false
    }
  })
  // First visit: open a tab for every live pane already running there.
  const ws = get().workspaces[key]
  if (!ws || ws.tabs.length === 0) {
    const panes = panesOfWorktree(get().panes[hostId], worktree).sort((a, b) => a.id - b.id)
    for (const p of panes) openPaneInTab(hostId, worktree, p.id, { activate: false })
    const first = get().workspaces[key]?.tabs[0]
    if (first) updateWorkspace(key, (w) => ({ ...w, activeTab: first.id }))
  }
}

export function findTabOfPane(hostId: string, pane: PaneId): { key: string; tab: Tab } | null {
  for (const [key, ws] of Object.entries(get().workspaces)) {
    if (!key.startsWith(`${hostId}\u0000`)) continue
    const tab = ws.tabs.find((t) => leaves(t.root).includes(pane))
    if (tab) return { key, tab }
  }
  return null
}

export function openPaneInTab(
  hostId: string,
  worktree: string,
  pane: PaneId,
  opts: { activate?: boolean } = {}
): void {
  const existing = findTabOfPane(hostId, pane)
  if (existing) {
    if (opts.activate !== false) {
      updateWorkspace(existing.key, (w) => ({ ...w, activeTab: existing.tab.id }))
    }
    return
  }
  const tab: Tab = { id: newTabId(), root: leaf(pane), focused: pane }
  updateWorkspace(wsKey(hostId, worktree), (w) => ({
    tabs: [...w.tabs, tab],
    activeTab: opts.activate === false && w.activeTab ? w.activeTab : tab.id
  }))
}

/** Jump to a pane from a toast / notification. */
export function revealPane(hostId: string, worktree: string | null, pane: PaneId): void {
  if (!worktree) return
  selectWorktree(hostId, worktree)
  openPaneInTab(hostId, worktree, pane)
  focusPane(hostId, pane)
}

export function activateTab(key: string, tabId: string): void {
  updateWorkspace(key, (w) => ({ ...w, activeTab: tabId }))
}

/** Reorder a tab within its worktree (tab-strip drag); `toIndex` is clamped. */
export function moveTab(key: string, tabId: string, toIndex: number): void {
  updateWorkspace(key, (w) => {
    const from = w.tabs.findIndex((t) => t.id === tabId)
    const to = Math.max(0, Math.min(toIndex, w.tabs.length - 1))
    if (from < 0 || from === to) return w
    const tabs = [...w.tabs]
    const [tab] = tabs.splice(from, 1)
    tabs.splice(to, 0, tab!)
    return { ...w, tabs }
  })
}

export function focusPane(hostId: string, pane: PaneId): void {
  const found = findTabOfPane(hostId, pane)
  if (!found || found.tab.focused === pane) return
  updateWorkspace(found.key, (w) => ({
    ...w,
    tabs: w.tabs.map((t) => (t.id === found.tab.id ? { ...t, focused: pane } : t))
  }))
}

export function setSplitRatio(key: string, tabId: string, path: NodePath, ratio: number): void {
  updateWorkspace(key, (w) => ({
    ...w,
    tabs: w.tabs.map((t) => (t.id === tabId ? { ...t, root: setRatio(t.root, path, ratio) } : t))
  }))
}

function removePaneEverywhere(hostId: string, pane: PaneId): void {
  set((s) => {
    const panes = { ...s.panes[hostId] }
    delete panes[pane]
    const workspaces: Record<string, Workspace> = {}
    for (const [key, ws] of Object.entries(s.workspaces)) {
      if (!key.startsWith(`${hostId}\u0000`)) {
        workspaces[key] = ws
        continue
      }
      const tabs: Tab[] = []
      for (const t of ws.tabs) {
        const root = removeLeaf(t.root, pane)
        if (!root) continue
        const focused = leaves(root).includes(t.focused) ? t.focused : leaves(root)[0]!
        tabs.push(root === t.root ? t : { ...t, root, focused })
      }
      const activeTab = tabs.some((t) => t.id === ws.activeTab)
        ? ws.activeTab
        : (tabs[Math.max(0, ws.tabs.findIndex((t) => t.id === ws.activeTab) - 1)]?.id ??
          tabs[0]?.id ??
          null)
      workspaces[key] = { tabs, activeTab }
    }
    return { panes: { ...s.panes, [hostId]: panes }, workspaces }
  })
}

async function spawnShell(hostId: string, worktree: string): Promise<PaneInfo> {
  const wt = worktreesOf(hostId).find((w) => w.id === worktree)
  const pane = await backend().spawnPane(hostId, {
    worktree,
    cwd: wt?.path ?? null,
    command: null,
    env: [],
    size: DEFAULT_SPAWN_SIZE,
    title: null,
    agent: null
  })
  addPaneSnapshot(hostId, pane)
  return pane
}

export async function newTerminalTab(hostId: string, worktree: string): Promise<void> {
  if (isRemoving(hostId, worktree)) return
  try {
    const pane = await spawnShell(hostId, worktree)
    openPaneInTab(hostId, worktree, pane.id)
  } catch (err) {
    reportError('Could not open a terminal', err)
  }
}

export async function splitFocused(dir: SplitDirection): Promise<void> {
  const active = get().active
  if (!active || isRemoving(active.hostId, active.worktree)) return
  const key = wsKey(active.hostId, active.worktree)
  const ws = get().workspaces[key]
  const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
  if (!tab) return newTerminalTab(active.hostId, active.worktree)
  try {
    const pane = await spawnShell(active.hostId, active.worktree)
    updateWorkspace(key, (w) => ({
      ...w,
      tabs: w.tabs.map((t) =>
        t.id === tab.id
          ? { ...t, root: splitLeaf(t.root, t.focused, pane.id, dir), focused: pane.id }
          : t
      )
    }))
  } catch (err) {
    reportError('Could not split the pane', err)
  }
}

function isBusyAgent(p: PaneInfo | undefined): boolean {
  return !!p && !p.exited && (p.status === 'running' || p.status === 'needsInput')
}

/** Kill panes (hangup), asking first if any of them is an agent that is still working. */
export function closePanes(hostId: string, panes: PaneId[]): void {
  const info = get().panes[hostId] ?? {}
  const kill = () => {
    for (const pane of panes) {
      backend()
        .killPane(hostId, pane, 'hangup')
        .then(() => removePaneEverywhere(hostId, pane))
        .catch((err: unknown) => {
          // Already gone on the daemon: just drop it locally.
          if (CivetError.from(err).code === 'notFound') removePaneEverywhere(hostId, pane)
          else reportError('Could not close the pane', err)
        })
    }
  }
  const busy = panes.filter((p) => isBusyAgent(info[p]))
  if (busy.length === 0 || !usePrefs.getState().confirmCloseRunning) return kill()
  set({
    confirm: {
      title: 'Stop running agent?',
      body: `${busy.map((p) => info[p]?.title || `pane ${p}`).join(', ')} is still working. Closing sends SIGHUP to it.`,
      confirmLabel: 'Close anyway',
      onConfirm: kill
    }
  })
}

export function closeTab(key: string, tabId: string): void {
  const hostId = key.slice(0, key.indexOf('\u0000'))
  const tab = get().workspaces[key]?.tabs.find((t) => t.id === tabId)
  if (tab) closePanes(hostId, leaves(tab.root))
}

// ── Repos, worktrees and agents ───────────────────────────────────────────

export async function addRepo(hostId: string, path: string): Promise<RepoInfo> {
  const repo = await backend().addRepo(hostId, path)
  await refreshRepos(hostId)
  return repo
}

/**
 * "Add project": add a git repository on a host, starting This Mac's daemon
 * first when needed (no separate connect step), then open its main worktree.
 */
export async function addProject(hostId: string, path: string): Promise<void> {
  if (hostId === 'local' && get().hosts.local?.state !== 'connected') {
    await connectHost({ kind: 'local' }, 'auto')
  }
  const repo = await addRepo(hostId, path)
  const k = wtKey(hostId, repo.id)
  if (usePrefs.getState().collapsedRepos[k]) toggleRepoCollapsed(hostId, repo.id)
  const list = get().worktrees[k] ?? []
  const main = list.find((w) => w.isMain) ?? list[0]
  if (main) selectWorktree(hostId, main.id)
}

export type AgentChoice = 'claude' | 'codex' | 'custom'

export async function launchAgentInNewWorktree(p: {
  hostId: string
  repo: string
  branch: string
  base: string | null
  agent: AgentChoice
  command: CommandSpec
  title: string | null
}): Promise<void> {
  const { worktree, pane } = await backend().launchAgent(p.hostId, {
    repo: p.repo,
    branch: p.branch,
    base: p.base,
    worktreePath: null,
    command: p.command,
    env: [],
    size: DEFAULT_SPAWN_SIZE,
    title: p.title,
    agent: p.agent === 'custom' ? p.command.program.split('/').pop() || null : p.agent
  })
  addPaneSnapshot(p.hostId, pane)
  applyCreatedWorktree(p.hostId, worktree)
  selectWorktree(p.hostId, worktree.id)
  openPaneInTab(p.hostId, worktree.id, pane.id)
}

// ── v3: agent tabs, on-create steps, removal, navigation ──────────────────

/** Parse a command line typed by the user ("claude --model opus"). */
export function parseCommand(line: string): CommandSpec | null {
  const words = splitShellWords(line)
  return words.length ? { program: words[0]!, args: words.slice(1) } : null
}

/**
 * The agent hint for a command: its program name, or `kind` when the program
 * is some other launcher (`npx @openai/codex`), so the tab still gets the
 * agent's name and logo.
 */
const agentOf = (cmd: CommandSpec, kind?: AgentKind) => {
  const program = cmd.program.split('/').pop() || null
  return kind && program !== 'claude' && program !== 'codex' ? kind : program
}

/** New tab running an agent (⌥C / ⌥X / "+" → Claude Code, Codex) in an existing worktree. */
export async function newAgentTab(
  hostId: string,
  worktree: string,
  kind: AgentKind = 'claude'
): Promise<void> {
  if (isRemoving(hostId, worktree)) return
  try {
    const command = parseCommand(agentCommandFor(kind))
    if (!command) throw new Error('No agent command configured')
    const wt = worktreesOf(hostId).find((w) => w.id === worktree)
    const pane = await backend().spawnPane(hostId, {
      worktree,
      cwd: wt?.path ?? null,
      command,
      env: [],
      size: DEFAULT_SPAWN_SIZE,
      title: null,
      agent: agentOf(command, kind)
    })
    addPaneSnapshot(hostId, pane)
    openPaneInTab(hostId, worktree, pane.id)
  } catch (err) {
    reportError('Could not start the agent', err)
  }
}

/**
 * "New worktree" with on-create steps (v3): each step opens in its own tab, in
 * order. The first agent step goes through `launch_agent` (worktree + agent
 * in one call) and receives the optional first task as its last argument;
 * terminal steps are login shells that get the command typed into them.
 * Without an agent step the worktree is created with `create_worktree`.
 */
export async function createWorktreeWithSteps(p: {
  hostId: string
  repo: string
  branch: string
  base: string | null
  steps: CreateStep[]
  prompt: string
}): Promise<void> {
  const steps = p.steps
    .map((st) => {
      const agent = stepAgent(st.kind)
      return { ...st, agent, cmd: st.cmd.trim() || (agent ? agentCommandFor(agent) : '') }
    })
    .filter((st) => st.agent || st.cmd)
  const firstAgent = steps.findIndex((st) => st.agent)
  const prompt = p.prompt.trim()
  const specs = steps.map((st, i) => {
    if (!st.agent) return null
    const spec = parseCommand(st.cmd)
    if (!spec) throw new Error(`Step ${i + 1}: enter a command`)
    return i === firstAgent && prompt ? { ...spec, args: [...spec.args, prompt] } : spec
  })

  let worktree: WorktreeInfo
  const opened: PaneId[] = []
  const branch = p.branch.trim()
  if (!branch) {
    // No branch name: no new worktree, the steps run in the main checkout.
    const main = worktreesOf(p.hostId).find((w) => w.repo === p.repo && w.isMain)
    if (!main) throw new Error("This project's main checkout was not found")
    worktree = main
  } else if (firstAgent >= 0) {
    const command = specs[firstAgent]!
    const res = await backend().launchAgent(p.hostId, {
      repo: p.repo,
      branch,
      base: p.base,
      worktreePath: null,
      command,
      env: [],
      size: DEFAULT_SPAWN_SIZE,
      title: prompt ? prompt.slice(0, 80) : branch,
      agent: agentOf(command, steps[firstAgent]!.agent!)
    })
    worktree = res.worktree
    addPaneSnapshot(p.hostId, res.pane)
    opened[firstAgent] = res.pane.id
  } else {
    worktree = await backend().createWorktree(p.hostId, p.repo, branch, p.base, null)
  }
  if (branch) applyCreatedWorktree(p.hostId, worktree)

  for (const [i, st] of steps.entries()) {
    // With a new worktree the first agent was started by launch_agent.
    if (i === firstAgent && branch) continue
    try {
      const pane = await backend().spawnPane(p.hostId, {
        worktree: worktree.id,
        cwd: worktree.path,
        command: st.agent ? specs[i]! : null,
        env: [],
        size: DEFAULT_SPAWN_SIZE,
        title: i === firstAgent && prompt ? prompt.slice(0, 80) : null,
        agent: st.agent ? agentOf(specs[i]!, st.agent) : null
      })
      addPaneSnapshot(p.hostId, pane)
      opened[i] = pane.id
      if (!st.agent) await backend().writeInput(p.hostId, pane.id, `${st.cmd}\r`)
    } catch (err) {
      reportError(`On-create step ${i + 1} failed`, err)
    }
  }
  if (opened.filter((x) => x !== undefined).length === 0) {
    const shell = await spawnShell(p.hostId, worktree.id).catch(() => null)
    if (shell) opened.push(shell.id)
  }

  selectWorktree(p.hostId, worktree.id)
  for (const pane of opened)
    if (pane !== undefined) openPaneInTab(p.hostId, worktree.id, pane, { activate: false })
  const focus = opened[firstAgent >= 0 ? firstAgent : 0] ?? opened.find((x) => x !== undefined)
  if (focus !== undefined) openPaneInTab(p.hostId, worktree.id, focus)
  const label = steps.length
    ? steps.map((st) => (st.agent ? `opened ${st.cmd}` : `ran ${st.cmd}`)).join(', ')
    : 'no on-create steps'
  const what = branch ? `Created ${branch}` : `Opened in ${worktreeBranch(worktree)}`
  pushToast({ kind: 'info', title: `${what} · ${label}` }, 3200)
}

/**
 * Remove a worktree on the host, then close the panes that ran in it. Runs in
 * the background: only this worktree is marked `removing` (its sidebar row,
 * tabs and panes are disabled) while the rest of the app stays usable. A
 * second call for the same worktree while one is in flight is ignored.
 * Failures are reported as a toast; git's refusal is kept so reopening the
 * dialog offers "Remove anyway". A worktree the host no longer knows
 * (removed from a terminal meanwhile) counts as removed. Either way the
 * repo is re-listed so the sidebar matches git.
 */
export async function removeWorktree(
  hostId: string,
  worktree: string,
  force: boolean
): Promise<void> {
  const key = wsKey(hostId, worktree)
  if (get().removing[key]) return
  const wt = worktreesOf(hostId).find((w) => w.id === worktree)
  const name = wt?.branch ?? worktree
  set((s) => ({ removing: { ...s.removing, [key]: true } }))
  const closePanes = () => {
    for (const pane of panesOfWorktree(get().panes[hostId], worktree)) {
      backend()
        .killPane(hostId, pane.id, 'hangup')
        .catch(() => {})
        .finally(() => removePaneEverywhere(hostId, pane.id))
    }
  }
  // Why: a forced removal deletes files under running sessions; an agent
  // still writing there makes git fail with "Directory not empty". Stop
  // them first. A plain removal may be refused (dirty), so its sessions
  // are only closed once git has succeeded.
  if (force) closePanes()
  const settle = (refused: string | null) =>
    set((s) => {
      const removing = { ...s.removing }
      delete removing[key]
      const removeRefused = { ...s.removeRefused }
      if (refused === null) delete removeRefused[key]
      else removeRefused[key] = refused
      return { removing, removeRefused }
    })
  try {
    await backend().removeWorktree(hostId, worktree, force)
  } catch (err) {
    const e = CivetError.from(err)
    if (e.code !== 'notFound') {
      settle(e.message)
      reportError(`Could not remove ${name}`, err)
      if (wt) void refreshWorktrees(hostId, wt.repo)
      return
    }
  }
  settle(null)
  if (wt) bumpWorktreeEpoch(hostId, wt.repo)
  if (usePrefs.getState().archivedWorktrees[key]) {
    setWorktreeArchived(hostId, worktree, false)
  }
  closePanes()
  set((s) => {
    const workspaces = { ...s.workspaces }
    delete workspaces[key]
    const worktrees = { ...s.worktrees }
    if (wt) {
      const k = wtKey(hostId, wt.repo)
      worktrees[k] = (worktrees[k] ?? []).filter((w) => w.id !== worktree)
    }
    return { workspaces, worktrees, mru: s.mru.filter((k) => k !== key) }
  })
  // Why: the removal can take a while; if the user moved to another worktree
  // meanwhile, leave their selection alone.
  const a = get().active
  if (a?.hostId === hostId && a.worktree === worktree) {
    const main = worktreesOf(hostId).find((w) => w.isMain && (!wt || w.repo === wt.repo))
    if (main) selectWorktree(hostId, main.id)
    else set({ active: null })
  }
  if (wt) void refreshWorktrees(hostId, wt.repo)
  pushToast(
    { kind: 'info', title: `Removed ${name} on ${get().hosts[hostId]?.label ?? hostId}` },
    3200
  )
}

/**
 * All worktrees in sidebar order (hosts, then repos), optionally skipping
 * collapsed repos and archived worktrees.
 */
export function allWorktrees(
  opts: { visibleOnly?: boolean } = {}
): { hostId: string; wt: WorktreeInfo }[] {
  const s = get()
  const { archivedWorktrees: archived, collapsedHosts, collapsedRepos } = usePrefs.getState()
  return sortedHostIds(s.hostOrder)
    .filter((hostId) => !opts.visibleOnly || !collapsedHosts[hostId])
    .flatMap((hostId) =>
      sortedRepos(hostId, s.repos[hostId] ?? [])
        .filter((r) => !opts.visibleOnly || !collapsedRepos[wtKey(hostId, r.id)])
        .flatMap((r) => (s.worktrees[wtKey(hostId, r.id)] ?? []).map((wt) => ({ hostId, wt })))
        .filter((x) => !opts.visibleOnly || !archived[wsKey(hostId, x.wt.id)])
    )
}

/**
 * Archive: hide a worktree in its project's collapsed "Archived" group. Files
 * and panes are untouched; the flag is a client pref (this machine only).
 * Archiving the selected worktree moves the selection to the repo's main one.
 */
export function setWorktreeArchived(hostId: string, worktree: string, archived: boolean): void {
  const key = wsKey(hostId, worktree)
  const archivedWorktrees = { ...usePrefs.getState().archivedWorktrees }
  if (archived) archivedWorktrees[key] = true
  else delete archivedWorktrees[key]
  setPrefs({ archivedWorktrees })
  const a = get().active
  if (archived && a?.hostId === hostId && a.worktree === worktree) {
    const wt = worktreesOf(hostId).find((w) => w.id === worktree)
    const main = worktreesOf(hostId).find((w) => w.isMain && (!wt || w.repo === wt.repo))
    if (main) selectWorktree(hostId, main.id)
  }
}

/** ⌥` : back to the previously selected worktree. */
export function selectPreviousWorktree(): void {
  const s = get()
  const cur = s.active ? wsKey(s.active.hostId, s.active.worktree) : null
  const all = allWorktrees()
  for (const key of s.mru) {
    if (key === cur) continue
    const hit = all.find((x) => wsKey(x.hostId, x.wt.id) === key)
    if (hit) return selectWorktree(hit.hostId, hit.wt.id)
  }
}

/** ⌥↑ / ⌥↓ : previous / next worktree in the sidebar. */
export function cycleWorktree(dir: 1 | -1): void {
  const list = allWorktrees({ visibleOnly: true })
  if (!list.length) return
  const a = get().active
  const i = a ? list.findIndex((x) => x.hostId === a.hostId && x.wt.id === a.worktree) : -1
  const next = list[(i + dir + list.length) % list.length]!
  selectWorktree(next.hostId, next.wt.id)
}

/** ⌥1…⌥9 : activate the n-th tab of the active worktree. */
export function activateTabIndex(i: number): void {
  const a = get().active
  if (!a) return
  const key = wsKey(a.hostId, a.worktree)
  const tab = get().workspaces[key]?.tabs[i]
  if (tab) activateTab(key, tab.id)
}

export function toggleRepoCollapsed(hostId: string, repo: string): void {
  const k = wtKey(hostId, repo)
  const collapsedRepos = { ...usePrefs.getState().collapsedRepos }
  if (collapsedRepos[k]) delete collapsedRepos[k]
  else collapsedRepos[k] = true
  setPrefs({ collapsedRepos })
}

export function toggleHostCollapsed(hostId: string): void {
  const collapsedHosts = { ...usePrefs.getState().collapsedHosts }
  if (collapsedHosts[hostId]) delete collapsedHosts[hostId]
  else collapsedHosts[hostId] = true
  setPrefs({ collapsedHosts })
}

/** Hosts in sidebar order (the user's drag order, then new hosts). */
export function sortedHostIds(hostOrder: readonly string[]): string[] {
  return applyOrder(hostOrder, usePrefs.getState().hostOrder)
}

/** A host's repos in sidebar order. */
export function sortedRepos(hostId: string, repos: readonly RepoInfo[]): RepoInfo[] {
  const byId = new Map(repos.map((r) => [r.id, r]))
  return applyOrder(
    repos.map((r) => r.id),
    usePrefs.getState().repoOrder[hostId]
  ).map((id) => byId.get(id)!)
}
