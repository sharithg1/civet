// Worktree sidebar, laid out like Cursor's: it runs the full window height,
// with the traffic lights, sidebar toggle and previous / next worktree
// arrows on top, then the nav (New worktree ⌥N, Search ⌘K, Add project ⌘O),
// then one section per host (label and state; latency in the dot's tooltip) holding its projects
// (repos) and their worktree rows: a status dot, the task / branch label and
// the age of the newest session. Under the selected worktree, its tabs.
// Settings (and an Update pill when a daemon upgrade is waiting) at the
// bottom. Hosts and projects collapse on a click of their header and reorder
// by dragging it (lib/reorder.ts); the layout is saved in the prefs. Data is
// the daemon's (repos, worktrees, panes); the "task" label is the agent
// pane's title (see lib/worktree-view.ts). Right-clicking a worktree row
// clears its label (branch / task name and PR badge, until they change),
// archives it (into the project's collapsed "Archived" group) or removes it
// (the Remove worktree dialog), and updates it from the default branch
// (rebase or merge, state/lifecycle.ts). At rest a row keeps to its label,
// PR state and age; hovering swaps the age for how far the branch is ahead
// of / behind the default branch and its agent spend. A project with stale worktrees
// (merged, remote branch gone, PR merged or closed) shows a Clean up button
// with their count. While a removal runs the row is dimmed,
// shows a spinner and ignores clicks and the context menu. A folder project
// (not a git repository) is a single row that selects its root: it has no
// worktrees. ⌘-click picks several worktrees for "Send prompt" (⇧⌘B; a bar
// above Settings shows the selection), and prompts queued on a worktree's
// agents (state/prompts.ts) are listed under its row: click to edit, × to
// cancel.

import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  Archive,
  ArchiveRestore,
  ListEnd,
  ArrowLeft,
  ArrowRight,
  Brush,
  ChevronDown,
  ChevronRight,
  Eraser,
  Folder,
  FolderPlus,
  GitBranchPlus,
  GitMerge,
  Lock,
  MessageSquare,
  MoreHorizontal,
  PanelLeft,
  Plus,
  Search,
  Send,
  Settings,
  SquarePen,
  Terminal,
  Trash2,
  X
} from 'lucide-react'
import {
  activateTab,
  activateTabIndex,
  cycleWorktree,
  disconnectHost,
  isFolderRepo,
  openPullRequest,
  panesOfWorktree,
  reconnectHost,
  requestRestartDaemon,
  selectWorktree,
  setWorktreeArchived,
  setWorktreeLabelCleared,
  toggleHostCollapsed,
  toggleRepoCollapsed,
  useStore,
  worktreesOf,
  wsKey,
  wtKey
} from '@/state/store'
import type { HostView } from '@/state/store'
import { applyOrder, clampSidebarWidth, DEFAULT_PREFS, setPrefs, usePrefs } from '@/state/prefs'
import {
  aheadBehindLabel,
  aheadBehindTitle,
  updateFromMain,
  useLifecycle,
  useLifecycleSupported,
  useRepoLifecycle,
  useWorktreeLifecycle
} from '@/state/lifecycle'
import { useChanges } from '@/state/changes'
import {
  cancelQueuedPrompt,
  clearSelected,
  openPromptDialog,
  openSendPrompt,
  toggleSelected,
  usePrompts
} from '@/state/prompts'
import { promptPreview, triggerLabel } from '@/lib/prompts'
import { dropLineClass, useReorder, type Reorder } from '@/lib/reorder'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { basename, cn } from '@/lib/utils'
import { useTitleBarInset } from '@/lib/window-chrome'
import {
  agentName,
  ago,
  clearedParts,
  worktreeBranch,
  worktreeDot,
  worktreeTask,
  type ClearedLabel
} from '@/lib/worktree-view'
import type { PaneId, PaneInfo, PullRequestInfo, QueuedPrompt, WorktreeInfo } from '@/types'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger
} from '@/components/ui/context-menu'
import { Spinner } from '@/components/ui/spinner'
import {
  checksSummary,
  PR_DETAIL_BACKGROUND_POLL_MS,
  reviewSummary,
  usePullRequestPolling,
  usePullRequests
} from '@/state/pull-requests'
import { Kbd, useWorktreeTabs } from './chrome'
import { CHECK_STATE, PR_STATE } from './PullRequestSection'
import { IconButton } from './IconButton'
import { PanelResizeHandle } from './PanelResizeHandle'
import { StatusDot } from './StatusDot'
import { AgentLogo, hasAgentLogo } from './AgentLogo'
import { WorktreeSpend } from './AgentUsage'

export function Sidebar() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const savedOrder = usePrefs((s) => s.hostOrder)
  const ids = applyOrder(
    hostOrder.filter((id) => hosts[id]),
    savedOrder
  )
  const reorder = useReorder(ids, (next) => setPrefs({ hostOrder: next }))
  const savedWidth = usePrefs((s) => s.sidebarWidth)
  // While dragging the width lives here; it is saved once, on release.
  const [dragWidth, setDragWidth] = useState<number | null>(null)
  const width = dragWidth ?? clampSidebarWidth(savedWidth)
  return (
    <aside
      className="relative flex h-full shrink-0 flex-col border-r border-border bg-sidebar text-sidebar-foreground"
      style={{ width }}
    >
      <PanelResizeHandle
        edge="right"
        label="Resize sidebar"
        width={width}
        clamp={clampSidebarWidth}
        onDrag={setDragWidth}
        onCommit={(w) => setPrefs({ sidebarWidth: w })}
        onReset={() => setPrefs({ sidebarWidth: DEFAULT_PREFS.sidebarWidth })}
      />
      <SidebarTop />
      <SidebarNav />
      <div
        ref={reorder.listRef}
        className="min-h-0 flex-1 overflow-y-auto pt-3 pb-3"
        data-sidebar-scroll
      >
        {ids.map((id, i) => (
          <HostSection key={id} host={hosts[id]!} reorder={reorder} index={i} />
        ))}
      </div>
      <SelectionBar />
      <SidebarFooter />
    </aside>
  )
}

/** Window-title row: traffic-light inset, sidebar toggle, worktree history arrows. */
function SidebarTop() {
  const inset = useTitleBarInset()
  const backendKind = useStore((s) => s.backend?.kind)
  const hasActive = useStore((s) => !!s.active)
  return (
    <div
      data-tauri-drag-region="deep"
      data-titlebar-inset={inset}
      className="flex h-10 shrink-0 items-center gap-0.5 pr-2 pl-2 select-none"
      style={inset ? { paddingLeft: inset } : undefined}
    >
      <SidebarToggle />
      {backendKind === 'mock' && (
        <span
          className="ml-1.5 rounded-sm bg-status-wait/15 px-1.5 font-mono text-2xs text-status-wait"
          title="Not running inside Tauri: using the in-browser mock backend"
        >
          mock
        </span>
      )}
      <span className="flex-1" />
      <IconButton
        label="Previous worktree (⌥↑)"
        side="bottom"
        disabled={!hasActive}
        onClick={() => cycleWorktree(-1)}
      >
        <ArrowLeft className="size-4" />
      </IconButton>
      <IconButton
        label="Next worktree (⌥↓)"
        side="bottom"
        disabled={!hasActive}
        onClick={() => cycleWorktree(1)}
      >
        <ArrowRight className="size-4" />
      </IconButton>
    </div>
  )
}

export function SidebarToggle() {
  const open = useStore((s) => s.sidebarOpen)
  return (
    <IconButton
      label={open ? 'Hide sidebar (⌥B)' : 'Show sidebar (⌥B)'}
      side="bottom"
      aria-pressed={open}
      onClick={() => useStore.setState((s) => ({ sidebarOpen: !s.sidebarOpen }))}
    >
      <PanelLeft className="size-4" />
    </IconButton>
  )
}

/** Where "New worktree" goes: the selected worktree's host and repo, else the first connected host. */
function newWorktreeTarget(): { hostId: string; repo?: string } | null {
  const s = useStore.getState()
  const a = s.active
  if (a && s.hosts[a.hostId]?.state === 'connected') {
    // A folder project's root has no worktrees: the dialog picks a repo.
    return { hostId: a.hostId, repo: worktreesOf(a.hostId).find((w) => w.id === a.worktree)?.repo }
  }
  const hostId = s.hostOrder.find((h) => s.hosts[h]?.state === 'connected')
  return hostId ? { hostId } : null
}

function SidebarNav() {
  const anyConnected = useStore((s) => s.hostOrder.some((h) => s.hosts[h]?.state === 'connected'))
  return (
    <nav className="flex shrink-0 flex-col gap-px px-2 pt-1">
      <NavItem
        icon={<SquarePen />}
        label="New worktree"
        hint="⌥N"
        disabled={!anyConnected}
        onClick={() => {
          const target = newWorktreeTarget()
          if (target) useStore.setState({ newWorktree: target })
        }}
      />
      <NavItem
        icon={<Search />}
        label="Search"
        hint="⌘K"
        onClick={() => useStore.setState({ switcherOpen: true })}
      />
      <NavItem
        icon={<FolderPlus />}
        label="Add project"
        hint="⌘O"
        onClick={() => useStore.setState({ addProject: { hostId: null } })}
      />
      <NavItem
        icon={<Send />}
        label="Send prompt"
        hint="⇧⌘B"
        disabled={!anyConnected}
        onClick={() => openSendPrompt()}
      />
    </nav>
  )
}

function NavItem({
  icon,
  label,
  hint,
  disabled,
  onClick
}: {
  icon: ReactNode
  label: string
  hint?: string
  disabled?: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="group flex h-[30px] cursor-pointer items-center gap-2.5 rounded-md px-2 text-left text-base text-soft hover:bg-hover hover:text-foreground disabled:cursor-default disabled:opacity-50 disabled:hover:bg-transparent [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-quiet"
    >
      {icon}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {hint && <Kbd className="opacity-0 transition-opacity group-hover:opacity-100">{hint}</Kbd>}
    </button>
  )
}

/** Worktrees ⌘-clicked for "Send prompt": how many, send, clear. */
function SelectionBar() {
  const count = usePrompts((s) => s.selected.length)
  if (!count) return null
  return (
    <div
      data-selection-bar
      title={`${count} worktree${count === 1 ? '' : 's'} selected (⌘-click to add or remove)`}
      className="mx-2 mb-1 flex h-[30px] shrink-0 items-center gap-1 rounded-md bg-sidebar-accent pr-1 pl-2 text-sm"
    >
      <span className="min-w-0 flex-1 truncate text-soft">{count} selected</span>
      <button
        type="button"
        onClick={() => openSendPrompt()}
        className="flex h-6 shrink-0 cursor-pointer items-center gap-1.5 rounded-[4px] px-1.5 text-foreground hover:bg-hover-strong [&_svg]:size-3.5"
      >
        <Send />
        Send prompt…
      </button>
      <IconButton label="Clear selection" side="top" onClick={clearSelected}>
        <X className="size-3.5" />
      </IconButton>
    </div>
  )
}

function SidebarFooter() {
  const upgradeHost = useStore((s) => s.hostOrder.find((h) => s.hosts[h]?.upgrade))
  const upgrade = useStore((s) => (upgradeHost ? s.hosts[upgradeHost]?.upgrade : null))
  return (
    <div className="flex h-12 shrink-0 items-center gap-2 px-2">
      <button
        type="button"
        onClick={() => useStore.setState({ settings: 'general' })}
        className="flex h-[30px] min-w-0 flex-1 cursor-pointer items-center gap-2.5 rounded-md px-2 text-base text-soft hover:bg-hover hover:text-foreground"
        title="Settings (⌘,)"
      >
        <Settings className="size-4 shrink-0 text-quiet" />
        <span className="truncate">Settings</span>
      </button>
      {upgradeHost && upgrade && (
        <button
          type="button"
          title={`civetd v${upgrade.runningVersion} running, v${upgrade.availableVersion} available: restart the daemon to upgrade`}
          onClick={() => requestRestartDaemon(upgradeHost)}
          className="h-6 shrink-0 cursor-pointer rounded-full bg-status-run px-2.5 text-sm font-medium text-white hover:brightness-110"
        >
          Update
        </button>
      )}
    </div>
  )
}

export function hostMeta(host: HostView): string {
  switch (host.state) {
    case 'connected':
      return host.rttMs != null
        ? `${host.rttMs} ms`
        : host.transport === 'stream'
          ? 'ssh'
          : 'connected'
    case 'connecting':
      return 'connecting'
    case 'reconnecting':
      return 'reconnecting'
    case 'failed':
      return 'failed'
    default:
      return 'offline'
  }
}

export function HostStateDot({ host }: { host: HostView }) {
  const s = host.state
  const label = s === 'connected' ? `connected · ${hostMeta(host)}` : s
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      data-host-state={s}
      className={cn(
        'inline-block size-1.5 shrink-0 rounded-full',
        s === 'connected' && 'bg-status-ok',
        (s === 'reconnecting' || s === 'connecting') && 'bg-status-wait',
        s === 'failed' && 'bg-status-err',
        s === 'disconnected' && 'border-[1.5px] border-dashed border-dim'
      )}
    />
  )
}

/** Small icon button that only shows while its row is hovered (or its menu is open). */
const ROW_ACTION =
  'grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-quiet opacity-0 group-hover:opacity-100 hover:bg-hover-strong hover:text-foreground focus-visible:opacity-100 data-[state=open]:opacity-100 disabled:cursor-default disabled:opacity-0 [&_svg]:size-3.5'

function HostSection({
  host,
  reorder,
  index
}: {
  host: HostView
  reorder: Reorder
  index: number
}) {
  const hostId = host.hostId
  const repos = useStore((s) => s.repos[hostId])
  const collapsed = usePrefs((s) => !!s.collapsedHosts[hostId])
  const savedRepoOrder = usePrefs((s) => s.repoOrder[hostId])
  const byId = new Map((repos ?? []).map((r) => [r.id, r]))
  const repoIds = applyOrder(
    (repos ?? []).map((r) => r.id),
    savedRepoOrder
  )
  const repoReorder = useReorder(repoIds, (next) =>
    setPrefs({ repoOrder: { ...usePrefs.getState().repoOrder, [hostId]: next } })
  )
  return (
    <section
      data-reorder-id={hostId}
      className={cn(
        'relative mb-3',
        dropLineClass(reorder.indicator(index)),
        reorder.dragging === hostId && 'opacity-50'
      )}
    >
      <div
        role="button"
        tabIndex={-1}
        aria-expanded={!collapsed}
        title="Click to collapse, drag to reorder"
        onPointerDown={reorder.handle(hostId)}
        onClick={() => toggleHostCollapsed(hostId)}
        className="group mx-2 flex h-7 cursor-default items-center gap-2 px-2"
      >
        <span className="truncate text-sm text-quiet" title={host.hostId}>
          {host.label}
        </span>
        <HostStateDot host={host} />
        {collapsed && <ChevronRight aria-hidden className="size-3 shrink-0 text-dim" />}
        {host.upgrade && (
          <span
            className="font-mono text-2xs text-status-wait"
            title={`civetd v${host.upgrade.runningVersion} running, v${host.upgrade.availableVersion} available: restart the daemon to upgrade`}
          >
            ↑ v{host.upgrade.availableVersion}
          </span>
        )}
        <span className="flex-1" />
        {host.state !== 'connected' && (
          <span className="font-mono text-2xs text-dim group-hover:hidden">{hostMeta(host)}</span>
        )}
        <button
          type="button"
          aria-label={`Add project on ${host.label}`}
          title="Add project…"
          onClick={(e) => {
            e.stopPropagation()
            useStore.setState({ addProject: { hostId } })
          }}
          className={cn(ROW_ACTION, 'hidden group-hover:grid')}
        >
          <FolderPlus />
        </button>
        <DropdownMenu>
          <DropdownMenuTrigger
            aria-label={`${host.label} actions`}
            onClick={(e) => e.stopPropagation()}
            className={cn(ROW_ACTION, '-mr-1.5 hidden group-hover:grid data-[state=open]:grid')}
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-52" onCloseAutoFocus={restoreTerminalFocus}>
            <DropdownMenuItem onSelect={() => useStore.setState({ addProject: { hostId } })}>
              Add project…
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => useStore.setState({ showConnect: true })}>
              Connect another host…
            </DropdownMenuItem>
            {(host.state === 'failed' || host.state === 'disconnected') && (
              <DropdownMenuItem onSelect={() => void reconnectHost(hostId)}>
                Reconnect
              </DropdownMenuItem>
            )}
            {(host.state === 'connected' || host.upgrade || host.errorCode === 'daemonBusy') && (
              <DropdownMenuItem onSelect={() => requestRestartDaemon(hostId)}>
                {host.upgrade
                  ? `Restart daemon (v${host.upgrade.availableVersion})…`
                  : 'Restart daemon…'}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onSelect={() => void disconnectHost(hostId)}>
              Disconnect
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {!collapsed && host.transport === 'stream' && (
        <div className="mx-2 px-2 pb-1 text-2xs text-dim">via SSH tunnel (UDP unavailable)</div>
      )}
      {!collapsed && (
        <div ref={repoReorder.listRef}>
          {repoIds.map((id, i) =>
            isFolderRepo(byId.get(id)) ? (
              <FolderProjectRow
                key={id}
                host={host}
                repoId={id}
                name={byId.get(id)!.name}
                reorder={repoReorder}
                index={i}
              />
            ) : (
              <ProjectGroup
                key={id}
                host={host}
                repoId={id}
                name={byId.get(id)!.name}
                reorder={repoReorder}
                index={i}
              />
            )
          )}
        </div>
      )}
      {!collapsed && repos && repos.length === 0 && (
        <button
          type="button"
          className="mx-2 flex h-[30px] w-[calc(100%-16px)] cursor-pointer items-center gap-2.5 rounded-md px-2 text-base text-quiet hover:bg-hover hover:text-foreground"
          onClick={() => useStore.setState({ addProject: { hostId } })}
        >
          <Plus className="size-4 shrink-0" />
          Add a project…
        </button>
      )}
    </section>
  )
}

/**
 * A folder project: one row (its root) that selects it, drags to reorder,
 * and has no "+": no worktrees or sessions, only file tabs.
 */
function FolderProjectRow({
  host,
  repoId,
  name,
  reorder,
  index
}: {
  host: HostView
  repoId: string
  name: string
  reorder: Reorder
  index: number
}) {
  const hostId = host.hostId
  const wt = useStore((s) => s.worktrees[wtKey(hostId, repoId)]?.[0])
  const selected = useStore(
    (s) => !!wt && s.active?.hostId === hostId && s.active.worktree === wt.id
  )
  return (
    <div
      data-reorder-id={repoId}
      className={cn(
        'relative',
        dropLineClass(reorder.indicator(index)),
        reorder.dragging === repoId && 'opacity-50'
      )}
    >
      <div
        role="button"
        tabIndex={-1}
        data-wt={wt?.id}
        data-folder="true"
        data-active={selected ? 'true' : 'false'}
        title={`${name} · folder (no git)\n${host.label}:${wt?.path ?? ''}`}
        onPointerDown={reorder.handle(repoId)}
        onClick={() => wt && selectWorktree(hostId, wt.id)}
        className={cn(
          'mx-2 mb-1 flex h-[30px] cursor-default items-center gap-2.5 rounded-md px-2',
          selected ? 'bg-sidebar-accent' : 'hover:bg-hover'
        )}
      >
        <Folder className="size-4 shrink-0 text-quiet" />
        <span
          className={cn(
            'min-w-0 flex-1 truncate text-base',
            selected ? 'text-foreground' : 'text-soft'
          )}
        >
          {name}
        </span>
      </div>
    </div>
  )
}

function ProjectGroup({
  host,
  repoId,
  name,
  reorder,
  index
}: {
  host: HostView
  repoId: string
  name: string
  reorder: Reorder
  index: number
}) {
  const hostId = host.hostId
  const worktrees = useStore((s) => s.worktrees[wtKey(hostId, repoId)])
  const collapsed = usePrefs((s) => !!s.collapsedRepos[wtKey(hostId, repoId)])
  useRepoLifecycle(hostId, repoId, host.state === 'connected')
  const stale = useStaleCount(hostId, repoId)
  const archivedKeys = usePrefs((s) => s.archivedWorktrees)
  const [showArchived, setShowArchived] = useState(false)
  const list = worktrees ?? []
  const live = list.filter((wt) => !archivedKeys[wsKey(hostId, wt.id)])
  const archived = list.filter((wt) => archivedKeys[wsKey(hostId, wt.id)])
  // Selecting an archived worktree (from the switcher, say) opens the group,
  // again on every re-selection: `seen` is stamped by each selectWorktree.
  const archivedSelectedAt = useStore((s) =>
    s.active?.hostId === hostId && archived.some((wt) => wt.id === s.active!.worktree)
      ? s.seen[wsKey(hostId, s.active.worktree)]
      : undefined
  )
  useEffect(() => {
    if (archivedSelectedAt != null) setShowArchived(true)
  }, [archivedSelectedAt])
  return (
    <div
      data-reorder-id={repoId}
      className={cn(
        'relative',
        dropLineClass(reorder.indicator(index)),
        reorder.dragging === repoId && 'opacity-50'
      )}
    >
      <div
        role="button"
        tabIndex={-1}
        aria-expanded={!collapsed}
        title="Click to collapse, drag to reorder"
        onPointerDown={reorder.handle(repoId)}
        onClick={() => toggleRepoCollapsed(hostId, repoId)}
        className="group mx-2 flex h-[30px] cursor-default items-center gap-2.5 rounded-md px-2 hover:bg-hover"
      >
        {/* Cursor's disclosure: a folder when collapsed, a chevron when open
            (and on hover, so the click target reads as a toggle). */}
        <span className="grid size-4 shrink-0 place-items-center text-quiet">
          {collapsed ? (
            <>
              <Folder className="size-4 group-hover:hidden" />
              <ChevronRight className="hidden size-4 group-hover:block" />
            </>
          ) : (
            <ChevronDown className="size-4" />
          )}
        </span>
        <span className="min-w-0 flex-1 truncate text-base text-soft">{name}</span>
        {stale > 0 && (
          <button
            type="button"
            title={`Clean up ${stale} stale worktree${stale === 1 ? '' : 's'} in ${name}`}
            aria-label={`Clean up worktrees in ${name}`}
            disabled={host.state !== 'connected'}
            onClick={(e) => {
              e.stopPropagation()
              useLifecycle.setState({ cleanup: { hostId, repo: repoId } })
            }}
            className="flex h-5 shrink-0 cursor-pointer items-center gap-1 rounded-md px-1.5 text-sm text-quiet hover:bg-hover-strong hover:text-foreground disabled:cursor-default disabled:opacity-50 [&_svg]:size-3"
          >
            <Brush />
            {stale}
          </button>
        )}
        <button
          type="button"
          title={`New worktree in ${name} (⌥N)`}
          aria-label={`New worktree in ${name}`}
          disabled={host.state !== 'connected'}
          onClick={(e) => {
            e.stopPropagation()
            useStore.setState({ newWorktree: { hostId, repo: repoId } })
          }}
          className={cn(ROW_ACTION, '-mr-1')}
        >
          <Plus />
        </button>
      </div>
      {!collapsed && (
        <div className="mb-1 flex flex-col gap-px">
          {live.map((wt) => (
            <WorktreeRow key={wt.id} host={host} wt={wt} />
          ))}
          {archived.length > 0 && (
            <>
              <button
                type="button"
                aria-expanded={showArchived}
                onClick={() => setShowArchived((v) => !v)}
                className="mx-2 flex h-[30px] cursor-pointer items-center gap-2.5 rounded-md px-2 text-base text-quiet hover:bg-hover hover:text-foreground"
              >
                <span className="grid size-4 shrink-0 place-items-center">
                  <Archive className="size-3.5" />
                </span>
                <span className="flex-1 text-left">Archived</span>
                <span className="text-sm text-dim">{archived.length}</span>
                <ChevronRight
                  className={cn(
                    'size-3.5 text-dim transition-transform',
                    showArchived && 'rotate-90'
                  )}
                />
              </button>
              {showArchived &&
                archived.map((wt) => <WorktreeRow key={wt.id} host={host} wt={wt} archived />)}
            </>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * Worktrees of a project that look stale: from the daemon's lifecycle
 * (merged, remote branch deleted) or their PR's state (merged, closed). The
 * Clean up dialog checks again, with a fetch.
 */
function useStaleCount(hostId: string, repo: string): number {
  const k = wtKey(hostId, repo)
  const life = useLifecycle((l) => l.byRepo[k])
  const prs = useStore((s) => s.pullRequests[k])
  const list = useStore((s) => s.worktrees[k])
  const defaultBranch = useStore((s) => s.repos[hostId]?.find((r) => r.id === repo)?.defaultBranch)
  if (!life) return 0
  return life.worktrees.filter((w) => {
    if (w.stale.length) return true
    if (!w.branch || w.branch === defaultBranch) return false
    if (list?.find((x) => x.id === w.worktree)?.isMain) return false
    const state = prs?.[w.branch]?.state
    return state === 'merged' || state === 'closed'
  }).length
}

/** Right-click menu of a worktree row: send prompt, clear label, archive / unarchive, update, remove. */
function WorktreeMenu({
  hostId,
  wt,
  archived,
  removing,
  label,
  children
}: {
  hostId: string
  wt: WorktreeInfo
  archived: boolean
  removing: boolean
  /** What the row shows now, for "Clear label"; null when it shows nothing to clear. */
  label: ClearedLabel | null
  children: ReactNode
}) {
  const life = useWorktreeLifecycle(hostId, wt.repo, wt.id)
  const supported = useLifecycleSupported(hostId)
  const busy = useChanges((c) => !!c.busy[wsKey(hostId, wt.id)])
  const defaultBranch = useStore(
    (s) => s.repos[hostId]?.find((r) => r.id === wt.repo)?.defaultBranch
  )
  const base = life?.base ?? defaultBranch ?? 'main'
  // Why not the main worktree: it is usually on the default branch (Pull
  // updates that), and a feature branch there is rare.
  const canUpdate = !!life && !!wt.branch && wt.branch !== defaultBranch && !wt.isMain && !busy
  const behind = life?.info.behind ?? 0
  return (
    <ContextMenu>
      {/* Why: not `disabled`, which would let the webview's native menu
          through; preventing the event also keeps Radix's menu closed. */}
      <ContextMenuTrigger asChild onContextMenu={(e) => removing && e.preventDefault()}>
        {children}
      </ContextMenuTrigger>
      <ContextMenuContent className="w-56" onCloseAutoFocus={restoreTerminalFocus}>
        <ContextMenuItem onSelect={() => openSendPrompt([wsKey(hostId, wt.id)])}>
          <Send />
          Send prompt…
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem
          disabled={!label}
          onSelect={() => label && setWorktreeLabelCleared(hostId, wt.id, label)}
        >
          <Eraser />
          Clear label
        </ContextMenuItem>
        <ContextMenuItem
          disabled={wt.isMain}
          onSelect={() => setWorktreeArchived(hostId, wt.id, !archived)}
        >
          {archived ? <ArchiveRestore /> : <Archive />}
          {archived ? 'Unarchive' : 'Archive'}
        </ContextMenuItem>
        {supported && (
          <>
            <ContextMenuSeparator />
            {canUpdate && behind === 0 ? (
              <ContextMenuItem disabled>
                <GitBranchPlus />
                Up to date with {base}
              </ContextMenuItem>
            ) : (
              <>
                <ContextMenuItem
                  disabled={!canUpdate}
                  onSelect={() => void updateFromMain(hostId, wt.id, 'rebase')}
                >
                  <GitBranchPlus />
                  Rebase onto {base}
                </ContextMenuItem>
                <ContextMenuItem
                  disabled={!canUpdate}
                  onSelect={() => void updateFromMain(hostId, wt.id, 'merge')}
                >
                  <GitMerge />
                  Merge {base}
                </ContextMenuItem>
              </>
            )}
            <ContextMenuItem
              onSelect={() => useLifecycle.setState({ cleanup: { hostId, repo: wt.repo } })}
            >
              <Brush />
              Clean up worktrees…
            </ContextMenuItem>
          </>
        )}
        <ContextMenuSeparator />
        <ContextMenuItem
          variant="destructive"
          disabled={wt.isMain}
          onSelect={() => useStore.setState({ removeWorktree: { hostId, worktree: wt.id } })}
        >
          <Trash2 />
          Remove worktree…
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  )
}

/**
 * The branch's pull request state, as in Cursor's sidebar, with its CI state
 * and unresolved review comments while it is open; opens it on GitHub.
 */
function PullRequestIcon({
  pr,
  hostId,
  worktree
}: {
  pr: PullRequestInfo
  hostId: string
  worktree: string
}) {
  const { Icon, className, label } = PR_STATE[pr.state]
  const live = pr.state === 'open' || pr.state === 'draft'
  usePullRequestPolling(hostId, worktree, PR_DETAIL_BACKGROUND_POLL_MS, { enabled: live })
  const detail = usePullRequests((s) => {
    const d = s.byWorktree[wsKey(hostId, worktree)]?.detail
    return live && d?.pr.number === pr.number ? d : null
  })
  const checks = detail?.checksState ? CHECK_STATE[detail.checksState] : null
  const unresolved = detail?.unresolvedThreads ?? 0
  const lines = [`#${pr.number} · ${label}`, pr.title]
  if (detail?.checksState) lines.push(`${checks?.label}: ${checksSummary(detail)}`)
  const review = detail && reviewSummary(detail)
  if (review) lines.push(review)
  if (unresolved > 0) lines.push(`${unresolved} unresolved comment${unresolved === 1 ? '' : 's'}`)
  lines.push('Click to open on GitHub')
  return (
    <button
      type="button"
      aria-label={`Open PR #${pr.number} on GitHub`}
      title={lines.join('\n')}
      onClick={(e) => {
        e.stopPropagation()
        void openPullRequest(pr)
      }}
      className="-mx-1 flex h-[22px] shrink-0 cursor-pointer items-center gap-1 rounded-[4px] px-1 hover:bg-hover-strong"
    >
      {unresolved > 0 && (
        <span className="flex items-center gap-0.5 font-mono text-2xs text-quiet">
          <MessageSquare className="size-3" />
          {unresolved}
        </span>
      )}
      {checks && detail?.checksState !== 'passed' && (
        <checks.Icon aria-label={checks.label} className={cn('size-3', checks.className)} />
      )}
      <Icon className={cn('size-3.5', className)} />
    </button>
  )
}

function WorktreeRow({
  host,
  wt,
  archived = false
}: {
  host: HostView
  wt: WorktreeInfo
  archived?: boolean
}) {
  const hostId = host.hostId
  const key = wsKey(hostId, wt.id)
  const selected = useStore((s) => s.active?.hostId === hostId && s.active.worktree === wt.id)
  const unread = useStore((s) => s.unread[key])
  const removing = useStore((s) => !!s.removing[key])
  const picked = usePrompts((s) => s.selected.includes(key))
  const paneMap = useStore((s) => s.panes[hostId])
  // Any worktree, the main one included (a feature branch may be checked
  // out there), except on the default branch.
  const pr = useStore((s) => {
    const defaultBranch = s.repos[hostId]?.find((r) => r.id === wt.repo)?.defaultBranch
    if (!wt.branch || wt.branch === defaultBranch) return undefined
    return s.pullRequests[wtKey(hostId, wt.repo)]?.[wt.branch]
  })
  const life = useWorktreeLifecycle(hostId, wt.repo, wt.id)
  const counts = life ? aheadBehindLabel(life.info.ahead, life.info.behind) : ''
  const activeTab = useStore((s) => s.workspaces[key]?.activeTab ?? null)
  const labelPref = usePrefs((s) => s.worktreeLabel)
  const tabsPref = usePrefs((s) => s.sidebarTabs)
  // File tabs stay in the title bar only: the sidebar lists sessions.
  const tabs = useWorktreeTabs(hostId, wt.id).filter((t) => t.file == null)
  const panes = panesOfWorktree(paneMap, wt.id)
  const dot = worktreeDot(panes, host.state === 'connected', unread)
  const task = wt.isMain ? null : worktreeTask(wt, panes)
  const current: ClearedLabel = {
    branch: wt.branch,
    task,
    pr: pr ? `${pr.number}:${pr.state}` : null
  }
  const cleared = usePrefs((s) => s.clearedLabels[key])
  const hidden = clearedParts(cleared, current)
  useEffect(() => {
    if (hidden.stale) setWorktreeLabelCleared(hostId, wt.id, null)
  }, [hidden.stale, hostId, wt.id])
  // A cleared branch falls back to the worktree's folder name.
  const branch = hidden.branch ? basename(wt.path) : worktreeBranch(wt)
  const useTask = labelPref === 'task' && !!task && !hidden.task
  const title = useTask ? task! : branch
  const clearable =
    (current.branch != null && !hidden.branch) ||
    (task != null && !hidden.task) ||
    (pr != null && !hidden.pr)
  const newest = panes.reduce<number | null>(
    (m, p) => (m == null || p.createdAtMs > m ? p.createdAtMs : m),
    null
  )
  const showTabs = tabs.length > 0 && (tabsPref === 'all' || (tabsPref === 'selected' && selected))
  // Scroll into view on every selection, re-selecting this row included.
  const selectedAt = useStore((s) => (selected ? s.seen[key] : undefined))
  const rowRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (selectedAt != null) rowRef.current?.scrollIntoView({ block: 'nearest' })
  }, [selectedAt])
  return (
    <WorktreeMenu
      hostId={hostId}
      wt={wt}
      archived={archived}
      removing={removing}
      label={clearable ? current : null}
    >
      <div
        ref={rowRef}
        role="button"
        tabIndex={-1}
        data-wt={wt.id}
        data-archived={archived ? 'true' : undefined}
        data-active={selected ? 'true' : 'false'}
        data-removing={removing ? 'true' : undefined}
        data-picked={picked ? 'true' : undefined}
        aria-disabled={removing || undefined}
        title={
          removing
            ? `Removing ${host.label}:${wt.path}…`
            : `${useTask ? `${branch}\n` : ''}${host.label}:${wt.path}${wt.isMain ? '\nDefault worktree' : ''}`
        }
        onClick={(e) => {
          if (removing) return
          // ⌘-click: pick for "Send prompt"; a plain click drops the picks.
          if (e.metaKey || e.ctrlKey) return toggleSelected(key)
          clearSelected()
          selectWorktree(hostId, wt.id)
        }}
        className={cn(
          'group/row relative mx-2 flex cursor-default flex-col rounded-md px-2',
          selected ? 'bg-sidebar-accent' : !removing && 'hover:bg-hover',
          picked && 'ring-1 ring-status-run/70 ring-inset',
          archived && !selected && 'opacity-60',
          // Why: the tab sub-rows have their own click handlers; with their
          // pointer events off every click lands on the (guarded) row.
          removing && 'opacity-50 [&_*]:pointer-events-none'
        )}
      >
        <div className="flex h-[30px] items-center gap-2.5">
          <span className="grid size-4 shrink-0 place-items-center">
            {removing ? (
              <Spinner aria-label="Removing" className="size-3.5 text-faint" />
            ) : (
              <StatusDot
                kind={dot}
                size={6}
                showIdle
                className={cn(dot === 'idle' && 'border-0 bg-ghost')}
              />
            )}
          </span>
          <span
            className={cn(
              'min-w-0 flex-1 truncate',
              useTask || wt.isMain ? 'text-base' : 'font-mono text-sm',
              selected ? 'text-foreground' : unread ? 'text-foreground' : 'text-soft'
            )}
          >
            {title}
          </span>
          {wt.locked && <Lock aria-label="locked" className="size-3.5 shrink-0 text-dim" />}
          {counts && life?.base && !removing && (
            <span
              data-ahead-behind
              title={aheadBehindTitle(life.info.ahead, life.info.behind, life.base)}
              className="sr-only shrink-0 font-mono text-2xs text-quiet group-hover/row:not-sr-only"
            >
              {counts}
            </span>
          )}
          {pr && !hidden.pr && !removing && (
            <PullRequestIcon pr={pr} hostId={hostId} worktree={wt.id} />
          )}
          {!removing && (
            <span className="sr-only group-hover/row:not-sr-only">
              <WorktreeSpend hostId={hostId} worktree={wt.id} />
            </span>
          )}
          {removing ? (
            <span className="shrink-0 text-sm text-quiet">Removing…</span>
          ) : (
            newest != null && (
              <span className="shrink-0 text-sm text-quiet group-hover/row:hidden">
                {ago(newest)}
              </span>
            )
          )}
        </div>
        {showTabs && (
          <div className="-mt-0.5 mb-1 flex flex-col">
            {tabs.map((t, i) => {
              const isActive = selected && (t.id ? t.id === activeTab : i === 0)
              return (
                <div
                  key={t.id ?? `p${t.focused}`}
                  title={`${t.label} · ${t.what.text}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    selectWorktree(hostId, wt.id)
                    if (t.id) activateTab(key, t.id)
                    else activateTabIndex(i)
                  }}
                  className="group/tab flex h-6 cursor-default items-center gap-2 pl-[26px] text-sm"
                >
                  {t.agent && hasAgentLogo(t.agent.agent) ? (
                    <AgentLogo agent={t.agent.agent} className="size-3" />
                  ) : t.agent ? null : (
                    <Terminal className="size-3 shrink-0 text-dim" />
                  )}
                  <span
                    className={cn(
                      'shrink-0',
                      isActive ? 'text-foreground' : 'text-quiet group-hover/tab:text-soft'
                    )}
                  >
                    {t.label}
                  </span>
                  <span
                    className={cn(
                      'min-w-0 flex-1 truncate text-dim',
                      t.what.mono && 'font-mono text-xs'
                    )}
                  >
                    {t.what.text}
                  </span>
                </div>
              )
            })}
          </div>
        )}
        {!removing && <QueuedPrompts hostId={hostId} panes={panes} />}
      </div>
    </WorktreeMenu>
  )
}

/**
 * Prompts queued on a worktree's agents, next first: one line each (click
 * to edit, × to cancel), the agent named when the worktree has several.
 */
function QueuedPrompts({ hostId, panes }: { hostId: string; panes: PaneInfo[] }) {
  const queues = usePrompts((s) => s.queues[hostId])
  const items: { prompt: QueuedPrompt; pane: PaneInfo }[] = []
  for (const pane of panes) {
    for (const prompt of queues?.[pane.id as PaneId] ?? []) items.push({ prompt, pane })
  }
  if (!items.length) return null
  const several = new Set(items.map((i) => i.pane.id)).size > 1
  // Named like the tabs: "Claude 1", "Codex 1"… in pane order.
  const names = new Map<PaneId, string>()
  const counts = new Map<string, number>()
  for (const p of panes.filter((p) => p.agent).sort((a, b) => a.id - b.id)) {
    const kind = agentName(p)
    const n = (counts.get(kind) ?? 0) + 1
    counts.set(kind, n)
    names.set(p.id, `${kind} ${n}`)
  }
  return (
    <div className="-mt-0.5 mb-1 flex flex-col" data-queued-prompts>
      {items.map(({ prompt, pane }, i) => {
        const next = i === 0 || items[i - 1]!.pane.id !== pane.id
        return (
          <div
            key={prompt.id}
            data-queued-prompt={prompt.id}
            title={`Queued for ${pane.title || `pane ${pane.id}`}, sent ${triggerLabel(prompt.trigger)}${next ? '' : ' (after the one above)'}\n\n${prompt.text}`}
            onClick={(e) => {
              e.stopPropagation()
              openPromptDialog({ kind: 'edit', hostId, prompt })
            }}
            className="group/qp flex h-6 cursor-default items-center gap-2 pl-[26px] text-sm"
          >
            <ListEnd className="size-3 shrink-0 text-dim" aria-label="Queued prompt" />
            {several && next && <span className="shrink-0 text-quiet">{names.get(pane.id)}</span>}
            <span className="min-w-0 flex-1 truncate text-quiet group-hover/qp:text-soft">
              {promptPreview(prompt.text, 60)}
            </span>
            <button
              type="button"
              aria-label="Cancel queued prompt"
              title="Cancel queued prompt"
              onClick={(e) => {
                e.stopPropagation()
                void cancelQueuedPrompt(hostId, prompt)
              }}
              className="grid size-[18px] shrink-0 cursor-pointer place-items-center rounded-[4px] text-dim opacity-0 group-hover/qp:opacity-100 hover:bg-hover-strong hover:text-foreground"
            >
              <X className="size-3" />
            </button>
          </div>
        )
      })}
    </div>
  )
}
