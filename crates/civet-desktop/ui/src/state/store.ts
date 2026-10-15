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
import type {
  AgentAttentionEvent,
  AgentStatus,
  BootstrapProgressEvent,
  BootstrapStep,
  CommandSpec,
  DaemonEvent,
  HostState,
  HostStateEvent,
  HostSummary,
  HostTarget,
  PaneId,
  PaneInfo,
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
}

export type NewWorktreeRequest = { hostId: string; repo?: string }

type State = {
  backend: CivetBackend | null
  hosts: Record<string, HostView>
  hostOrder: string[]
  repos: Record<string, RepoInfo[]>
  /** key: wtKey(hostId, repoId) */
  worktrees: Record<string, WorktreeInfo[]>
  panes: Record<string, Record<PaneId, PaneInfo>>
  /** key: wsKey(hostId, worktreeId) */
  workspaces: Record<string, Workspace>
  active: { hostId: string; worktree: string } | null
  /** key: wsKey; set when an agent wants attention outside the active worktree */
  unread: Record<string, AgentStatus>
  showConnect: boolean
  newWorktree: NewWorktreeRequest | null
  addRepoFor: string | null
  confirm: ConfirmRequest | null
  toasts: Toast[]
}

export const useStore = create<State>(() => ({
  backend: null,
  hosts: {},
  hostOrder: [],
  repos: {},
  worktrees: {},
  panes: {},
  workspaces: {},
  active: null,
  unread: {},
  showConnect: false,
  newWorktree: null,
  addRepoFor: null,
  confirm: null,
  toasts: []
}))

const set = useStore.setState
const get = useStore.getState

export const wtKey = (hostId: string, repo: string) => `${hostId}\u0000${repo}`
export const wsKey = (hostId: string, worktree: string) => `${hostId}\u0000${worktree}`

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
    b.onAgentAttention(onAgentAttention)
  ])
  const nudge = () => void b.notifyNetworkChange().catch(() => {})
  const onVisibility = () => document.visibilityState === 'visible' && nudge()
  window.addEventListener('online', nudge)
  document.addEventListener('visibilitychange', onVisibility)

  try {
    const hosts = await b.listHosts()
    for (const h of hosts) upsertHost(h)
    await Promise.all(hosts.filter((h) => h.state === 'connected').map((h) => refreshHost(h.hostId)))
    set({ showConnect: hosts.length === 0 })
  } catch (err) {
    reportError('Could not list hosts', err)
    set({ showConnect: true })
  }

  return () => {
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
  upsertHost(
    {
      hostId: e.hostId,
      label: prev?.label ?? e.hostId,
      state: e.state,
      transport: e.transport,
      daemonVersion: prev?.daemonVersion ?? null,
      instanceId: prev?.instanceId ?? null
    },
    {
      rttMs: e.rttMs,
      message: e.message,
      bootstrap: e.state === 'connected' ? null : (prev?.bootstrap ?? null)
    }
  )
  // After a re-bootstrap the daemon may be a new instance with different
  // panes; re-sync whenever we come back to connected.
  if (e.state === 'connected' && prevState && prevState !== 'connected') {
    void refreshHost(e.hostId)
  }
}

export function onBootstrapProgress(e: BootstrapProgressEvent): void {
  const prev = get().hosts[e.hostId]
  if (!prev) return
  upsertHost(prev, { bootstrap: { step: e.step, message: e.message } })
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
    patchPane(hostId, event.agentStatus.pane, { status: event.agentStatus.status })
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

async function refreshWorktrees(hostId: string, repo: string): Promise<void> {
  try {
    const list = await backend().listWorktrees(hostId, repo)
    set((s) => ({ worktrees: { ...s.worktrees, [wtKey(hostId, repo)]: list } }))
  } catch (err) {
    reportError(`Could not list worktrees of ${repo}`, err)
  }
}

async function refreshPanes(hostId: string): Promise<void> {
  const list = await backend().listPanes(hostId)
  const map: Record<PaneId, PaneInfo> = {}
  for (const p of list) map[p.id] = p
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

export async function connectHost(target: HostTarget, transport: TransportChoice): Promise<void> {
  const hostId = target.kind === 'local' ? 'local' : target.destination.trim()
  upsertHost({
    hostId,
    label: target.kind === 'local' ? 'This Mac' : hostId,
    state: 'connecting',
    transport: null,
    daemonVersion: null,
    instanceId: null
  })
  let summary: HostSummary
  try {
    summary = await backend().connectHost(target, transport)
  } catch (err) {
    const cur = get().hosts[hostId]
    if (cur && cur.state === 'connecting') {
      upsertHost(cur, { state: 'failed', message: CivetError.from(err).message, bootstrap: null })
    }
    throw err
  }
  upsertHost(summary, { bootstrap: null })
  await refreshHost(summary.hostId)
  set({ showConnect: false })
  // Focus the first worktree that has panes, else the first main worktree.
  if (!get().active) {
    const first = worktreesOf(summary.hostId)[0]
    if (first) selectWorktree(summary.hostId, first.id)
  }
}

export async function disconnectHost(hostId: string): Promise<void> {
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
    return { active: { hostId, worktree }, unread }
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

export function findTabOfPane(
  hostId: string,
  pane: PaneId
): { key: string; tab: Tab } | null {
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
  set((s) => ({ panes: { ...s.panes, [hostId]: { ...s.panes[hostId], [pane.id]: pane } } }))
  return pane
}

export async function newTerminalTab(hostId: string, worktree: string): Promise<void> {
  try {
    const pane = await spawnShell(hostId, worktree)
    openPaneInTab(hostId, worktree, pane.id)
  } catch (err) {
    reportError('Could not open a terminal', err)
  }
}

export async function splitFocused(dir: SplitDirection): Promise<void> {
  const active = get().active
  if (!active) return
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
  if (busy.length === 0) return kill()
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

export async function addRepo(hostId: string, path: string): Promise<void> {
  await backend().addRepo(hostId, path)
  await refreshRepos(hostId)
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
  set((s) => {
    const k = wtKey(p.hostId, worktree.repo)
    const list = s.worktrees[k] ?? []
    return {
      panes: { ...s.panes, [p.hostId]: { ...s.panes[p.hostId], [pane.id]: pane } },
      worktrees: {
        ...s.worktrees,
        [k]: list.some((w) => w.id === worktree.id) ? list : [...list, worktree]
      }
    }
  })
  selectWorktree(p.hostId, worktree.id)
  openPaneInTab(p.hostId, worktree.id, pane.id)
}
