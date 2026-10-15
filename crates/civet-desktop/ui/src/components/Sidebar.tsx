// Worktree sidebar (Civet Workspace v3): "Switch worktree…" button, then
// hosts → projects (repos) → worktree rows with a status dot, task / branch
// labels and, under the selected worktree, its tabs. "+" next to it adds a
// project (⌘O) on any host, This Mac included. Hosts and projects collapse
// on a click of their header and reorder by dragging it (lib/reorder.ts);
// the layout is saved in the prefs. Settings link at the
// bottom. Data is the daemon's (repos, worktrees, panes); the "task" label is
// the agent pane's title (see lib/worktree-view.ts). Right-clicking a worktree
// row archives it (into the project's collapsed "Archived" group) or removes
// it (the Remove worktree dialog).

import { useState, type ReactNode } from 'react'
import { Archive, ArchiveRestore, ChevronRight, MoreHorizontal, Plus, Trash2 } from 'lucide-react'
import {
  activateTab,
  activateTabIndex,
  disconnectHost,
  panesOfWorktree,
  reconnectHost,
  requestRestartDaemon,
  selectWorktree,
  setWorktreeArchived,
  toggleHostCollapsed,
  toggleRepoCollapsed,
  useStore,
  wsKey,
  wtKey
} from '@/state/store'
import type { HostView } from '@/state/store'
import { applyOrder, setPrefs, usePrefs } from '@/state/prefs'
import { dropLineClass, useReorder, type Reorder } from '@/lib/reorder'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import {
  splitBranch,
  worktreeBranch,
  worktreeDot,
  worktreeTask
} from '@/lib/worktree-view'
import type { WorktreeInfo } from '@/types'
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
import { Kbd, ProjectTile, RingGlyph, useWorktreeTabs } from './chrome'
import { StatusDot } from './StatusDot'

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
    <aside className="flex h-full w-[260px] shrink-0 flex-col border-r border-border bg-chrome">
      <div className="flex shrink-0 gap-1.5 px-2.5 pt-2.5 pb-1.5">
        <button
          type="button"
          onClick={() => useStore.setState({ switcherOpen: true })}
          className="flex h-8 min-w-0 flex-1 cursor-pointer items-center gap-2 rounded-lg border border-border bg-white/[0.045] px-2.5 text-left hover:bg-muted"
        >
          <RingGlyph />
          <span className="min-w-0 flex-1 truncate text-[13px] text-faint">Switch worktree…</span>
          <Kbd>⌘K</Kbd>
        </button>
        <button
          type="button"
          title="Add project (⌘O)"
          aria-label="Add project"
          onClick={() => useStore.setState({ addProject: { hostId: null } })}
          className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg border border-border bg-white/[0.045] text-quiet hover:bg-muted hover:text-foreground"
        >
          <Plus className="size-4" />
        </button>
      </div>
      <div ref={reorder.listRef} className="min-h-0 flex-1 overflow-y-auto pt-1 pb-3" data-sidebar-scroll>
        {ids.map((id, i) => (
          <HostSection key={id} host={hosts[id]!} reorder={reorder} index={i} />
        ))}
      </div>
      <div className="shrink-0 border-t border-border px-2 py-1.5">
        <button
          type="button"
          title="Settings (⌘,)"
          onClick={() => useStore.setState({ settings: 'general' })}
          className="flex h-7 w-full cursor-pointer items-center gap-2 rounded-md px-2 text-[12.5px] text-muted-foreground hover:bg-hover hover:text-foreground"
        >
          <span className="grid size-3 shrink-0 place-items-center rounded-full border-[1.5px] border-current">
            <span className="size-1 rounded-full bg-current" />
          </span>
          Settings
          <span className="flex-1" />
          <span className="font-mono text-[10.5px] text-dim">⌘,</span>
        </button>
      </div>
    </aside>
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

/** Disclosure chevron: points right when collapsed, down when open. */
function Chevron({ open, className }: { open: boolean; className?: string }) {
  return (
    <ChevronRight
      aria-hidden
      className={cn('size-3 shrink-0 text-dim transition-transform', open && 'rotate-90', className)}
    />
  )
}

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
        'relative mb-1.5',
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
        className="group flex cursor-default items-center gap-2 pt-2 pr-[18px] pb-1.5 pl-0.5"
      >
        <Chevron open={!collapsed} className={cn(!collapsed && 'opacity-0 group-hover:opacity-100')} />
        <span className="-ml-1 text-[11.5px] text-quiet" title={host.hostId}>
          {host.label}
        </span>
        <HostStateDot host={host} />
        {host.upgrade && (
          <span
            className="font-mono text-[10.5px] text-status-wait"
            title={`civetd v${host.upgrade.runningVersion} running, v${host.upgrade.availableVersion} available: restart the daemon to upgrade`}
          >
            ↑ v{host.upgrade.availableVersion}
          </span>
        )}
        <span className="flex-1" />
        <span className="font-mono text-[10.5px] text-dim">{hostMeta(host)}</span>
        <DropdownMenu>
          <DropdownMenuTrigger
            aria-label={`${host.label} actions`}
            className="-mr-1.5 grid size-4 cursor-pointer place-items-center rounded-sm text-faint opacity-0 group-hover:opacity-100 hover:bg-hover-strong hover:text-foreground focus-visible:opacity-100 data-[state=open]:opacity-100"
          >
            <MoreHorizontal className="size-3.5" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-48" onCloseAutoFocus={restoreTerminalFocus}>
            <DropdownMenuItem onSelect={() => useStore.setState({ addProject: { hostId: host.hostId } })}>
              Add project…
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => useStore.setState({ showConnect: true })}>
              Connect another host…
            </DropdownMenuItem>
            {(host.state === 'failed' || host.state === 'disconnected') && (
              <DropdownMenuItem onSelect={() => void reconnectHost(host.hostId)}>Reconnect</DropdownMenuItem>
            )}
            {(host.state === 'connected' || host.upgrade || host.errorCode === 'daemonBusy') && (
              <DropdownMenuItem onSelect={() => requestRestartDaemon(host.hostId)}>
                {host.upgrade ? `Restart daemon (v${host.upgrade.availableVersion})…` : 'Restart daemon…'}
              </DropdownMenuItem>
            )}
            <DropdownMenuItem onSelect={() => void disconnectHost(host.hostId)}>Disconnect</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {!collapsed && host.transport === 'stream' && (
        <div className="px-[18px] pb-1 text-[10.5px] text-dim">via SSH tunnel (UDP unavailable)</div>
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
          className="mx-[18px] cursor-pointer text-[12px] text-faint underline hover:text-foreground"
          onClick={() => useStore.setState({ addProject: { hostId: host.hostId } })}
        >
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
        className="group mx-2 flex cursor-default items-center gap-[9px] rounded-md px-2.5 py-[7px] hover:bg-hover"
      >
        <ProjectTile name={name} />
        <span className="truncate text-[13.5px] font-semibold text-foreground">{name}</span>
        <Chevron open={!collapsed} className={cn('-ml-1', !collapsed && 'opacity-0 group-hover:opacity-100')} />
        <span className="flex-1" />
        <button
          type="button"
          title={`New worktree in ${name} (⌥N)`}
          aria-label={`New worktree in ${name}`}
          disabled={host.state !== 'connected'}
          onClick={(e) => {
            e.stopPropagation()
            useStore.setState({ newWorktree: { hostId, repo: repoId } })
          }}
          className="-mr-1 grid size-5 shrink-0 cursor-pointer place-items-center rounded-sm text-[15px] leading-none text-quiet hover:bg-white/[0.08] hover:text-foreground disabled:cursor-default disabled:opacity-40"
        >
          +
        </button>
      </div>
      {!collapsed && (
        <div className="mt-0.5 mr-2 mb-2.5 ml-[18px] flex flex-col gap-px">
          {live.map((wt) => (
            <WorktreeRow key={wt.id} host={host} wt={wt} />
          ))}
          {archived.length > 0 && (
            <>
              <button
                type="button"
                aria-expanded={showArchived}
                onClick={() => setShowArchived((v) => !v)}
                className="mt-0.5 flex h-6 cursor-pointer items-center gap-1.5 rounded-md px-2 text-[11.5px] text-faint hover:bg-hover hover:text-foreground"
              >
                <ChevronRight className={cn('size-3 transition-transform', showArchived && 'rotate-90')} />
                Archived
                <span className="font-mono text-[10.5px] text-dim">{archived.length}</span>
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
  children
}: {
  hostId: string
  wt: WorktreeInfo
  archived: boolean
  children: ReactNode
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
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

function WorktreeRow({ host, wt, archived = false }: { host: HostView; wt: WorktreeInfo; archived?: boolean }) {
  const hostId = host.hostId
  const key = wsKey(hostId, wt.id)
  const selected = useStore((s) => s.active?.hostId === hostId && s.active.worktree === wt.id)
  const unread = useStore((s) => s.unread[key])
  const paneMap = useStore((s) => s.panes[hostId])
  const activeTab = useStore((s) => s.workspaces[key]?.activeTab ?? null)
  const labelPref = usePrefs((s) => s.worktreeLabel)
  const tabsPref = usePrefs((s) => s.sidebarTabs)
  const tabs = useWorktreeTabs(hostId, wt.id)
  const panes = panesOfWorktree(paneMap, wt.id)
  const dot = worktreeDot(panes, host.state === 'connected', unread)
  const branch = worktreeBranch(wt)
  const { prefix } = splitBranch(branch)
  const task = wt.isMain ? null : worktreeTask(wt, panes)
  const useTask = labelPref === 'task' && !!task
  const title = wt.isMain ? branch : useTask ? task! : branch
  const sub = useTask ? branch.slice(prefix.length) : task
  const showTabs = tabs.length > 0 && (tabsPref === 'all' || (tabsPref === 'selected' && selected))
  return (
    <WorktreeMenu hostId={hostId} wt={wt} archived={archived}>
      <div
        role="button"
        tabIndex={-1}
        data-wt={wt.id}
        data-archived={archived ? 'true' : undefined}
        data-active={selected ? 'true' : 'false'}
        title={`${host.label}:${wt.path}`}
        onClick={() => selectWorktree(hostId, wt.id)}
        className={cn(
          'relative flex cursor-default gap-[9px] rounded-md px-2',
          wt.isMain ? 'py-[5px]' : 'pt-[5px] pb-1.5',
          selected ? 'bg-accent' : 'hover:bg-hover',
          archived && !selected && 'opacity-60'
        )}
      >
        <StatusDot kind={dot} showIdle={dot !== 'idle'} className="mt-1.5" />
        <div className="min-w-0 flex-1">
          <div className="flex min-h-[19px] items-center gap-2">
            <span
              className={cn(
                'min-w-0 flex-1 truncate',
                useTask ? 'text-[13px]' : 'font-mono text-[12px]',
                selected || unread ? 'text-foreground' : 'text-soft',
                selected && 'font-medium'
              )}
            >
              {title}
            </span>
            {wt.isMain && <span className="shrink-0 text-[11px] text-faint">default</span>}
            {wt.locked && <span className="shrink-0 text-[11px] text-faint">locked</span>}
          </div>
          {selected && !wt.isMain && sub && (
            <div className={cn('truncate text-[11px] leading-4 text-faint', useTask && 'font-mono')}>
              {useTask && <span className="text-dim">{prefix}</span>}
              {sub}
            </div>
          )}
          {showTabs && (
            <div className="mt-1 flex flex-col">
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
                    className="flex h-[21px] cursor-default items-center gap-2 text-[12px] hover:opacity-80"
                  >
                    <span className={cn('shrink-0', isActive ? 'text-foreground' : 'text-muted-foreground')}>
                      {t.label}
                    </span>
                    <span
                      className={cn(
                        'min-w-0 flex-1 truncate text-faint',
                        t.what.mono ? 'font-mono text-[11px]' : 'text-[12px]'
                      )}
                    >
                      {t.what.text}
                    </span>
                  </div>
                )
              })}
            </div>
          )}
        </div>
      </div>
    </WorktreeMenu>
  )
}
