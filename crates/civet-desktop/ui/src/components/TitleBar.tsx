// Header of the main column (Cursor-style: the sidebar and the right panel
// each have their own header row, so this one only spans the work area). It
// holds a compact worktree selector (opens the switcher), the worktree's
// tabs as pills, a "+" menu (Claude Code / Codex / Terminal / splits), a "…" menu of
// worktree actions and, when the right panel is closed, its toggle. With the
// sidebar closed it also takes the traffic-light inset and the sidebar
// toggle. Tabs keep the old TabBar's activate / close / middle-click-close
// behaviour, plus drag to reorder; a tab is still a split tree of panes.
// On macOS the window uses an overlay title bar, so every non-interactive
// part of this row drags the window (double click zooms); see
// lib/window-chrome.ts.

import { useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import {
  ChevronDown,
  Copy,
  Diff,
  GitBranch,
  MoreHorizontal,
  PanelRight,
  Plus,
  Terminal,
  Trash2,
  X
} from 'lucide-react'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { useTitleBarInset } from '@/lib/window-chrome'
import {
  compactWorktreeName,
  paneDot,
  worktreeBranch,
  worktreeTask,
  type DotKind
} from '@/lib/worktree-view'
import {
  activateTab,
  closeTab,
  moveTab,
  newAgentTab,
  newTerminalTab,
  panesOfWorktree,
  pinTab,
  pushToast,
  splitFocused,
  useStore,
  wsKey,
  tabPanes
} from '@/state/store'
import { FileIcon } from './FilesTab'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'
import { useWorktreeInfo, useWorktreeTabs } from './chrome'
import { IconButton } from './IconButton'
import { SidebarToggle } from './Sidebar'
import { StatusDot } from './StatusDot'
import { AgentLogo, hasAgentLogo } from './AgentLogo'

export function TitleBar() {
  const sidebarOpen = useStore((s) => s.sidebarOpen)
  const rightPanelOpen = useStore((s) => s.rightPanelOpen)
  const active = useStore((s) => s.active)
  const inset = useTitleBarInset()
  return (
    <header
      // "deep": any non-interactive descendant (empty tab strip, spacer)
      // drags the window; buttons and tabs keep their clicks.
      data-tauri-drag-region="deep"
      data-titlebar-inset={sidebarOpen ? undefined : inset}
      className="flex h-10 shrink-0 items-center gap-1 bg-background pr-2 pl-3 select-none"
      style={!sidebarOpen && inset ? { paddingLeft: inset } : undefined}
    >
      {!sidebarOpen && (
        <div className="mr-1 flex shrink-0 items-center">
          <SidebarToggle />
        </div>
      )}
      {active && <WorktreeTitle hostId={active.hostId} worktree={active.worktree} />}
      {active && <Tabs hostId={active.hostId} worktree={active.worktree} />}
      <div className="min-w-4 flex-1 self-stretch" />
      {active && <WorktreeActions hostId={active.hostId} worktree={active.worktree} />}
      {!rightPanelOpen && <RightPanelToggle />}
    </header>
  )
}

export function RightPanelToggle() {
  const open = useStore((s) => s.rightPanelOpen)
  return (
    <IconButton
      label={open ? 'Hide panel (⌥E)' : 'Show panel (⌥E)'}
      side="bottom"
      aria-pressed={open}
      className="shrink-0"
      onClick={() => useStore.setState((s) => ({ rightPanelOpen: !s.rightPanelOpen }))}
    >
      <PanelRight className="size-4" />
    </IconButton>
  )
}

/**
 * Compact worktree selector: a short branch name (`codex-tabs`) that opens
 * the ⌘K switcher. The project, full branch, host and task are in the
 * tooltip; the sidebar already shows the hierarchy, and the tabs need the room.
 */
function WorktreeTitle({ hostId, worktree }: { hostId: string; worktree: string }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const panes = useStore((s) => s.panes[hostId])
  const hostLabel = useStore((s) => s.hosts[hostId]?.label ?? hostId)
  if (!wt) return null
  const { repo, info } = wt
  const branch = worktreeBranch(info)
  const task = info.isMain ? null : worktreeTask(info, panesOfWorktree(panes, worktree))
  const tooltip = [branch, `${repo.name} · ${hostLabel}`, task, 'Switch worktree (⌘K)']
    .filter(Boolean)
    .join('\n')
  return (
    <button
      type="button"
      title={tooltip}
      aria-label={`Worktree ${branch} in ${repo.name}. Switch worktree`}
      aria-haspopup="dialog"
      onClick={() => useStore.setState({ switcherOpen: true })}
      className="group -ml-1.5 flex h-7 max-w-[200px] min-w-0 shrink-0 cursor-pointer items-center gap-1.5 rounded-md px-1.5 text-quiet hover:bg-hover hover:text-foreground"
    >
      <GitBranch className="size-3.5 shrink-0 text-dim group-hover:text-quiet" />
      <span className="min-w-0 truncate font-mono text-sm">{compactWorktreeName(branch)}</span>
      <ChevronDown className="size-3.5 shrink-0 text-dim" />
    </button>
  )
}

/** "…" menu: copy the remote path, branch details, remove the worktree. */
function WorktreeActions({ hostId, worktree }: { hostId: string; worktree: string }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const host = useStore((s) => s.hosts[hostId])
  if (!wt) return null
  const { info } = wt
  const remote = `${host?.label ?? hostId}:${info.path}`
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label="Worktree actions"
        className="grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-muted-foreground outline-none hover:bg-hover-strong hover:text-foreground data-[state=open]:bg-hover-strong"
      >
        <MoreHorizontal className="size-4" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        sideOffset={4}
        className="w-56"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <DropdownMenuItem
          onSelect={() => {
            void navigator.clipboard?.writeText(info.path).catch(() => {})
            pushToast({ kind: 'info', title: `Copied ${remote}` }, 3200)
          }}
        >
          <Copy />
          Copy remote path
        </DropdownMenuItem>
        <DropdownMenuItem
          onSelect={() => useStore.setState({ rightPanelOpen: true, rightTab: 'branch' })}
        >
          <GitBranch />
          Branch details
        </DropdownMenuItem>
        {!info.isMain && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              variant="destructive"
              disabled={host?.state !== 'connected'}
              onSelect={() => useStore.setState({ removeWorktree: { hostId, worktree } })}
            >
              <Trash2 />
              Remove worktree…
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function Tabs({ hostId, worktree }: { hostId: string; worktree: string }) {
  const key = wsKey(hostId, worktree)
  const activeTab = useStore((s) => s.workspaces[key]?.activeTab ?? null)
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  const removing = useStore((s) => !!s.removing[key])
  const panes = useStore((s) => s.panes[hostId])
  const tabs = useWorktreeTabs(hostId, worktree)
  const wsTabs = useStore((s) => s.workspaces[key]?.tabs)
  const stripRef = useRef<HTMLDivElement>(null)
  const drag = useRef<{ id: string; startX: number; moved: boolean } | null>(null)
  const [dragging, setDragging] = useState<string | null>(null)

  // Pointer events rather than HTML5 drag-and-drop: Tauri's native file-drop
  // handler swallows HTML5 drag events in the webview. The strip reorders
  // live as the pointer crosses a neighbour's midpoint.
  const onPointerMove = (e: ReactPointerEvent) => {
    const d = drag.current
    if (!d) return
    if (!d.moved) {
      if (Math.abs(e.clientX - d.startX) < 4) return
      d.moved = true
      setDragging(d.id)
    }
    const els = Array.from(stripRef.current?.querySelectorAll<HTMLElement>('[role="tab"]') ?? [])
    const from = els.findIndex((el) => el.dataset.tabId === d.id)
    if (from < 0) return
    const mid = (el: HTMLElement) => {
      const r = el.getBoundingClientRect()
      return r.left + r.width / 2
    }
    const next = els[from + 1]
    const prev = els[from - 1]
    if (next && e.clientX > mid(next)) moveTab(key, d.id, from + 1)
    else if (prev && e.clientX < mid(prev)) moveTab(key, d.id, from - 1)
  }
  const endDrag = (e: ReactPointerEvent) => {
    const d = drag.current
    if (!d) return
    drag.current = null
    setDragging(null)
    ;(e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId)
    // A drag isn't a click: keep the trailing click from re-activating.
    if (d.moved) {
      const swallow = (ev: MouseEvent) => ev.stopPropagation()
      window.addEventListener('click', swallow, { capture: true, once: true })
      setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 0)
    }
  }

  return (
    <>
      {/* Why: a worktree being removed keeps its tabs visible but inert
          (no activate / close / reorder); the rest of the bar still works. */}
      <div
        ref={stripRef}
        className={cn(
          'no-scrollbar ml-2 flex min-w-0 items-center gap-0.5 overflow-x-auto',
          removing && 'opacity-50'
        )}
        role="tablist"
        inert={removing}
        aria-disabled={removing || undefined}
      >
        {tabs.map((t) => {
          if (!t.id) return null
          const id = t.id
          const isActive = id === activeTab
          const tab = wsTabs?.find((x) => x.id === id)
          const count = tab ? tabPanes(tab).length : 1
          return (
            <div
              key={id}
              role="tab"
              aria-selected={isActive}
              data-active={isActive ? 'true' : 'false'}
              data-tab-id={id}
              title={t.what.text}
              onClick={() => activateTab(key, id)}
              onDoubleClick={(e) => {
                if (!(e.target as HTMLElement).closest('button')) pinTab(key, id)
              }}
              onPointerDown={(e) => {
                if (e.button !== 0 || (e.target as HTMLElement).closest('button')) return
                drag.current = { id, startX: e.clientX, moved: false }
                e.currentTarget.setPointerCapture(e.pointerId)
              }}
              onPointerMove={onPointerMove}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              onMouseDown={(e) => {
                if (e.button === 1) e.preventDefault()
              }}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault()
                  closeTab(key, id)
                }
              }}
              className={cn(
                'group/tab relative flex h-7 shrink-0 cursor-default items-center gap-1.5 rounded-md pr-1 pl-2.5 text-base select-none',
                isActive ? 'bg-card text-foreground' : 'text-quiet hover:bg-hover hover:text-soft',
                dragging === id && 'z-10 cursor-grabbing bg-hover-strong text-foreground'
              )}
            >
              {t.agent && hasAgentLogo(t.agent.agent) ? (
                <AgentTabIcon
                  agent={t.agent.agent}
                  dot={paneDot(panes?.[t.agent.id], connected)}
                  active={isActive}
                />
              ) : t.agent ? (
                <span className="grid size-3.5 shrink-0 place-items-center">
                  <StatusDot kind={paneDot(panes?.[t.agent.id], connected)} size={6} />
                </span>
              ) : t.diff ? (
                <Diff className="size-3.5 shrink-0 text-quiet" />
              ) : t.file != null ? (
                <FileIcon path={t.file} className="size-3.5 shrink-0 text-quiet" />
              ) : (
                <Terminal className="size-3.5 shrink-0 text-quiet" />
              )}
              <span className={cn('whitespace-nowrap', t.preview && 'italic')}>{t.label}</span>
              {count > 1 && (
                <span className="text-sm text-dim" title={`${count} panes`}>
                  {count}
                </span>
              )}
              <button
                type="button"
                aria-label={`Close tab ${t.label}`}
                onClick={(e) => {
                  e.stopPropagation()
                  closeTab(key, id)
                }}
                className={cn(
                  'grid size-[18px] cursor-pointer place-items-center rounded-[4px] text-quiet hover:bg-hover-strong hover:text-foreground',
                  !isActive && 'opacity-0 group-hover/tab:opacity-100'
                )}
              >
                <X className="size-3" />
              </button>
            </div>
          )
        })}
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger
          title="New tab"
          aria-label="New tab"
          disabled={!connected || removing}
          className="ml-0.5 grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-quiet outline-none hover:bg-hover-strong hover:text-foreground disabled:cursor-default disabled:opacity-40 data-[state=open]:bg-hover-strong"
        >
          <Plus className="size-4" />
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          sideOffset={4}
          className="w-[200px]"
          onCloseAutoFocus={restoreTerminalFocus}
        >
          <DropdownMenuItem onSelect={() => void newAgentTab(hostId, worktree)}>
            Claude Code<DropdownMenuShortcut>⌥C</DropdownMenuShortcut>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void newAgentTab(hostId, worktree, 'codex')}>
            Codex<DropdownMenuShortcut>⌥X</DropdownMenuShortcut>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void newTerminalTab(hostId, worktree)}>
            Terminal<DropdownMenuShortcut>⌥T</DropdownMenuShortcut>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => void splitFocused('row')}>
            Split right<DropdownMenuShortcut>⌘D</DropdownMenuShortcut>
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void splitFocused('column')}>
            Split down<DropdownMenuShortcut>⇧⌘D</DropdownMenuShortcut>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  )
}

/** The agent's logo with its status as a corner badge (none when idle). */
function AgentTabIcon({
  agent,
  dot,
  active
}: {
  agent: string | null
  dot: DotKind
  active: boolean
}) {
  return (
    <span className="relative grid size-3.5 shrink-0 place-items-center">
      <AgentLogo agent={agent} className="size-3.5" />
      {dot !== 'idle' && (
        <StatusDot
          kind={dot}
          size={6}
          className={cn(
            'absolute -right-0.5 -bottom-0.5 ring-[1.5px]',
            active ? 'ring-card' : 'ring-background',
            dot === 'disc' && 'bg-background'
          )}
        />
      )}
    </span>
  )
}
