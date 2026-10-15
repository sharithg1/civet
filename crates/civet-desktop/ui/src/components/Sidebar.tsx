// Worktree sidebar, laid out like Cursor's: it runs the full window height,
// with the traffic lights, sidebar toggle and previous / next worktree
// arrows on top, then the nav (New worktree ⌥N, Search ⌘K, Add project ⌘O),
// then one section per host (label, state, latency) holding its projects
// (repos) and their worktree rows: a status dot, the task / branch label and
// the age of the newest session. Under the selected worktree, its tabs.
// Settings (and an Update pill when a daemon upgrade is waiting) at the
// bottom. Hosts and projects collapse on a click of their header and reorder
// by dragging it (lib/reorder.ts); the layout is saved in the prefs. Data is
// the daemon's (repos, worktrees, panes); the "task" label is the agent
// pane's title (see lib/worktree-view.ts). Right-clicking a worktree row
// archives it (into the project's collapsed "Archived" group) or removes it
// (the Remove worktree dialog). While a removal runs the row is dimmed,
// shows a spinner and ignores clicks and the context menu.

import { useState, type ReactNode } from 'react'
import {
  Archive,
  ArchiveRestore,
  ArrowLeft,
  ArrowRight,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderPlus,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  Lock,
  MoreHorizontal,
  PanelLeft,
  Plus,
  Search,
  Settings,
  SquarePen,
  Terminal,
  Trash2
} from 'lucide-react'
import {
  activateTab,
  activateTabIndex,
  cycleWorktree,
  disconnectHost,
  panesOfWorktree,
  reconnectHost,
  requestRestartDaemon,
  selectWorktree,
  setWorktreeArchived,
  toggleHostCollapsed,
  toggleRepoCollapsed,
  useStore,
  worktreesOf,
  wsKey,
  wtKey
} from '@/state/store'
import type { HostView } from '@/state/store'
import { applyOrder, setPrefs, usePrefs } from '@/state/prefs'
import { dropLineClass, useReorder, type Reorder } from '@/lib/reorder'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { useTitleBarInset } from '@/lib/window-chrome'
import { ago, worktreeBranch, worktreeDot, worktreeTask } from '@/lib/worktree-view'
import type { PullRequestInfo, WorktreeInfo } from '@/types'
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
import { Kbd, useWorktreeTabs } from './chrome'
import { IconButton } from './IconButton'
import { StatusDot } from './StatusDot'
import { ClaudeLogo } from './ClaudeLogo'

/** Sidebar width; the main header lines up with it when the sidebar is closed. */
export const SIDEBAR_WIDTH = 260

export function Sidebar() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const savedOrder = usePrefs((s) => s.hostOrder)
  const ids = applyOrder(
    hostOrder.filter((id) => hosts[id]),
    savedOrder
  )
  const reorder = useReorder(ids, (next) => setPrefs({ hostOrder: next }))
  return (
    <aside
      className="flex h-full shrink-0 flex-col border-r border-border bg-sidebar text-sidebar-foreground"
      style={{ width: SIDEBAR_WIDTH }}
    >
      <SidebarTop />
      <SidebarNav />
      <div ref={reorder.listRef} className="min-h-0 flex-1 overflow-y-auto pt-3 pb-3" data-sidebar-scroll>
        {ids.map((id, i) => (
          <HostSection key={id} host={hosts[id]!} reorder={reorder} index={i} />
        ))}
      </div>
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
      <IconButton label="Previous worktree (⌥↑)" side="bottom" disabled={!hasActive} onClick={() => cycleWorktree(-1)}>
        <ArrowLeft className="size-4" />
      </IconButton>
      <IconButton label="Next worktree (⌥↓)" side="bottom" disabled={!hasActive} onClick={() => cycleWorktree(1)}>
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
      <NavItem icon={<Search />} label="Search" hint="⌘K" onClick={() => useStore.setState({ switcherOpen: true })} />
      <NavItem
        icon={<FolderPlus />}
        label="Add project"
        hint="⌘O"
        onClick={() => useStore.setState({ addProject: { hostId: null } })}
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
      return host.rttMs != null ? `${host.rttMs} ms` : host.transport === 'stream' ? 'ssh' : 'connected'
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
  return (
    <span
      aria-label={s}
      title={s}
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

function HostSection({ host, reorder, index }: { host: HostView; reorder: Reorder; index: number }) {
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
        <span className="font-mono text-2xs text-dim group-hover:hidden">{hostMeta(host)}</span>
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
              <DropdownMenuItem onSelect={() => void reconnectHost(hostId)}>Reconnect</DropdownMenuItem>
            )}
            {(host.state === 'connected' || host.upgrade || host.errorCode === 'daemonBusy') && (
              <DropdownMenuItem onSelect={() => requestRestartDaemon(hostId)}>
                {host.upgrade ? `Restart daemon (v${host.upgrade.availableVersion})…` : 'Restart daemon…'}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onSelect={() => void disconnectHost(hostId)}>Disconnect</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {!collapsed && host.transport === 'stream' && (
        <div className="mx-2 px-2 pb-1 text-2xs text-dim">via SSH tunnel (UDP unavailable)</div>
      )}
      {!collapsed && (
        <div ref={repoReorder.listRef}>
          {repoIds.map((id, i) => (
            <ProjectGroup
              key={id}
              host={host}
              repoId={id}
              name={byId.get(id)!.name}
              reorder={repoReorder}
              index={i}
            />
          ))}
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
  const archivedKeys = usePrefs((s) => s.archivedWorktrees)
  const [showArchived, setShowArchived] = useState(false)
  const list = worktrees ?? []
  const live = list.filter((wt) => !archivedKeys[wsKey(hostId, wt.id)])
  const archived = list.filter((wt) => archivedKeys[wsKey(hostId, wt.id)])
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
                <ChevronRight className={cn('size-3.5 text-dim transition-transform', showArchived && 'rotate-90')} />
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

/** Right-click menu of a worktree row: archive / unarchive, remove. */
function WorktreeMenu({
  hostId,
  wt,
  archived,
  removing,
  children
}: {
  hostId: string
  wt: WorktreeInfo
  archived: boolean
  removing: boolean
  children: ReactNode
}) {
  return (
    <ContextMenu>
      {/* Why: not `disabled`, which would let the webview's native menu
          through; preventing the event also keeps Radix's menu closed. */}
      <ContextMenuTrigger asChild onContextMenu={(e) => removing && e.preventDefault()}>
        {children}
      </ContextMenuTrigger>
      <ContextMenuContent className="w-48" onCloseAutoFocus={restoreTerminalFocus}>
        <ContextMenuItem disabled={wt.isMain} onSelect={() => setWorktreeArchived(hostId, wt.id, !archived)}>
          {archived ? <ArchiveRestore /> : <Archive />}
          {archived ? 'Unarchive' : 'Archive'}
        </ContextMenuItem>
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

const PR_ICON = {
  open: { Icon: GitPullRequest, className: 'text-status-ok', label: 'Open' },
  draft: { Icon: GitPullRequestDraft, className: 'text-quiet', label: 'Draft' },
  merged: { Icon: GitMerge, className: 'text-pr-merged', label: 'Merged' },
  closed: { Icon: GitPullRequestClosed, className: 'text-status-err', label: 'Closed' }
} as const

/** The branch's pull request state, as in Cursor's sidebar. */
function PullRequestIcon({ pr }: { pr: PullRequestInfo }) {
  const { Icon, className, label } = PR_ICON[pr.state]
  const text = `#${pr.number} · ${label}\n${pr.title}`
  return (
    <span role="img" aria-label={text} title={text} className="grid shrink-0 place-items-center">
      <Icon className={cn('size-3.5', className)} />
    </span>
  )
}

function WorktreeRow({ host, wt, archived = false }: { host: HostView; wt: WorktreeInfo; archived?: boolean }) {
  const hostId = host.hostId
  const key = wsKey(hostId, wt.id)
  const selected = useStore((s) => s.active?.hostId === hostId && s.active.worktree === wt.id)
  const unread = useStore((s) => s.unread[key])
  const removing = useStore((s) => !!s.removing[key])
  const paneMap = useStore((s) => s.panes[hostId])
  // Any worktree, the main one included (a feature branch may be checked
  // out there), except on the default branch.
  const pr = useStore((s) => {
    const defaultBranch = s.repos[hostId]?.find((r) => r.id === wt.repo)?.defaultBranch
    if (!wt.branch || wt.branch === defaultBranch) return undefined
    return s.pullRequests[wtKey(hostId, wt.repo)]?.[wt.branch]
  })
  const activeTab = useStore((s) => s.workspaces[key]?.activeTab ?? null)
  const labelPref = usePrefs((s) => s.worktreeLabel)
  const tabsPref = usePrefs((s) => s.sidebarTabs)
  const tabs = useWorktreeTabs(hostId, wt.id)
  const panes = panesOfWorktree(paneMap, wt.id)
  const dot = worktreeDot(panes, host.state === 'connected', unread)
  const branch = worktreeBranch(wt)
  const task = wt.isMain ? null : worktreeTask(wt, panes)
  const useTask = labelPref === 'task' && !!task
  const title = wt.isMain ? branch : useTask ? task! : branch
  const newest = panes.reduce<number | null>((m, p) => (m == null || p.createdAtMs > m ? p.createdAtMs : m), null)
  const showTabs = tabs.length > 0 && (tabsPref === 'all' || (tabsPref === 'selected' && selected))
  return (
    <WorktreeMenu hostId={hostId} wt={wt} archived={archived} removing={removing}>
      <div
        role="button"
        tabIndex={-1}
        data-wt={wt.id}
        data-archived={archived ? 'true' : undefined}
        data-active={selected ? 'true' : 'false'}
        data-removing={removing ? 'true' : undefined}
        aria-disabled={removing || undefined}
        title={
          removing
            ? `Removing ${host.label}:${wt.path}…`
            : `${useTask ? `${branch}\n` : ''}${host.label}:${wt.path}`
        }
        onClick={() => !removing && selectWorktree(hostId, wt.id)}
        className={cn(
          'relative mx-2 flex cursor-default flex-col rounded-md px-2',
          selected ? 'bg-sidebar-accent' : !removing && 'hover:bg-hover',
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
              <StatusDot kind={dot} size={6} showIdle className={cn(dot === 'idle' && 'border-0 bg-ghost')} />
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
          {pr && !removing && <PullRequestIcon pr={pr} />}
          {removing ? (
            <span className="shrink-0 text-sm text-quiet">Removing…</span>
          ) : wt.isMain ? (
            <span className="shrink-0 text-sm text-dim">default</span>
          ) : (
            newest != null && <span className="shrink-0 text-sm text-quiet">{ago(newest)}</span>
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
                  {t.agent?.agent === 'claude' ? (
                    <ClaudeLogo className="size-3" />
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
                  <span className={cn('min-w-0 flex-1 truncate text-dim', t.what.mono && 'font-mono text-xs')}>
                    {t.what.text}
                  </span>
                </div>
              )
            })}
          </div>
        )}
      </div>
    </WorktreeMenu>
  )
}
