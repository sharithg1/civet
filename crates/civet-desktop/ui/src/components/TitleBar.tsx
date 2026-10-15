// Top bar of the v3 workspace: wordmark + sidebar toggle above the sidebar,
// then the current worktree (opens the switcher), the worktree's tabs, a "+"
// menu (Claude Code / Terminal / splits) and the right-panel toggle.
// Tabs replace the old TabBar: same activate / close / middle-click-close
// behaviour, plus drag to reorder; a tab is still a split tree of panes.
// On macOS the window uses an overlay title bar, so this row is also the
// window's title bar: the traffic lights sit in its left inset and every
// non-interactive part of it drags the window (double click zooms); see
// lib/window-chrome.ts.

import { useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import { leaves } from '@/layout/pane-tree'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { useTitleBarInset } from '@/lib/window-chrome'
import { paneDot, splitBranch, worktreeBranch, worktreeTask } from '@/lib/worktree-view'
import {
  activateTab,
  closeTab,
  moveTab,
  newAgentTab,
  newTerminalTab,
  panesOfWorktree,
  splitFocused,
  useStore,
  wsKey
} from '@/state/store'
import { usePrefs } from '@/state/prefs'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'
import { PanelGlyph, ProjectTile, useWorktreeInfo, useWorktreeTabs } from './chrome'
import { StatusDot } from './StatusDot'

export function TitleBar() {
  const sidebarOpen = useStore((s) => s.sidebarOpen)
  const backendKind = useStore((s) => s.backend?.kind)
  const active = useStore((s) => s.active)
  const inset = useTitleBarInset()
  return (
    <header
      // "deep": any non-interactive descendant (wordmark, empty tab strip,
      // spacer) drags the window; buttons and tabs keep their clicks.
      data-tauri-drag-region="deep"
      data-titlebar-inset={inset}
      className="flex h-10 shrink-0 items-stretch border-b border-border bg-chrome select-none"
    >
      <div
        className={cn('flex shrink-0 items-center gap-3 pr-3 pl-3.5', sidebarOpen && 'w-[260px]')}
        style={inset ? { paddingLeft: inset } : undefined}
      >
        <span className="text-sm font-semibold text-foreground">Civet</span>
        {backendKind === 'mock' && (
          <span
            className="font-mono text-2xs text-status-wait"
            title="Not running inside Tauri: using the in-browser mock backend"
          >
            mock
          </span>
        )}
        <button
          type="button"
          title="Toggle sidebar (⌥B)"
          aria-label="Toggle sidebar"
          aria-pressed={sidebarOpen}
          onClick={() => useStore.setState((s) => ({ sidebarOpen: !s.sidebarOpen }))}
          className="grid h-5 w-[22px] cursor-pointer place-items-center rounded-sm hover:bg-hover-strong"
        >
          <PanelGlyph side="left" />
        </button>
      </div>
      <div className="flex min-w-0 flex-1 items-stretch">
        {active && <WorktreeContext hostId={active.hostId} worktree={active.worktree} />}
        {active && <Tabs hostId={active.hostId} worktree={active.worktree} />}
        <div className="flex-1" />
        <button
          type="button"
          title="Toggle panel (⌥E)"
          aria-label="Toggle right panel"
          onClick={() => useStore.setState((s) => ({ rightPanelOpen: !s.rightPanelOpen }))}
          className="grid w-10 shrink-0 cursor-pointer place-items-center hover:bg-hover"
        >
          <PanelGlyph side="right" />
        </button>
      </div>
    </header>
  )
}

function WorktreeContext({ hostId, worktree }: { hostId: string; worktree: string }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const panes = useStore((s) => s.panes[hostId])
  const labelPref = usePrefs((s) => s.worktreeLabel)
  if (!wt) return null
  const { repo, info } = wt
  const task = info.isMain ? null : worktreeTask(info, panesOfWorktree(panes, worktree))
  const useTask = labelPref === 'task' && !!task
  const title = useTask ? task! : worktreeBranch(info)
  return (
    <button
      type="button"
      title="Switch worktree (⌘K)"
      onClick={() => useStore.setState({ switcherOpen: true })}
      className="flex max-w-[320px] min-w-0 shrink cursor-pointer items-center gap-[7px] border-r border-border px-3 text-sm hover:bg-hover"
    >
      <ProjectTile name={repo.name} size={14} />
      <span className="shrink-0 whitespace-nowrap text-muted-foreground">{repo.name}</span>
      <span className="shrink-0 text-dim">/</span>
      <span
        className={cn(
          'min-w-0 truncate font-medium text-foreground',
          !useTask && 'font-mono'
        )}
      >
        {useTask ? title : <BranchText branch={title} />}
      </span>
      <span className="shrink-0 text-[9px] text-faint">▼</span>
    </button>
  )
}

function BranchText({ branch }: { branch: string }) {
  const { prefix, name } = splitBranch(branch)
  return (
    <>
      {prefix && <span className="font-normal text-faint">{prefix}</span>}
      {name}
    </>
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
        className={cn('no-scrollbar flex min-w-0 items-stretch overflow-x-auto', removing && 'opacity-50')}
        role="tablist"
        inert={removing}
        aria-disabled={removing || undefined}
      >
        {tabs.map((t) => {
          if (!t.id) return null
          const id = t.id
          const isActive = id === activeTab
          const tab = wsTabs?.find((x) => x.id === id)
          const count = tab ? leaves(tab.root).length : 1
          return (
            <div
              key={id}
              role="tab"
              aria-selected={isActive}
              data-active={isActive ? 'true' : 'false'}
              data-tab-id={id}
              title={t.what.text}
              onClick={() => activateTab(key, id)}
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
                'relative flex shrink-0 cursor-default items-center gap-2 border-r border-border-subtle pr-2.5 pl-3 text-sm select-none hover:text-foreground',
                isActive ? 'bg-background text-foreground' : 'text-muted-foreground',
                dragging === id && 'z-10 cursor-grabbing bg-hover-strong text-foreground'
              )}
            >
              {t.agent ? (
                <StatusDot kind={paneDot(panes?.[t.agent.id], connected)} />
              ) : (
                <span className="font-mono text-2xs text-faint">›_</span>
              )}
              <span className="whitespace-nowrap">{t.label}</span>
              {count > 1 && (
                <span className="font-mono text-2xs text-faint" title={`${count} panes`}>
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
                className="size-4 cursor-pointer rounded-[3px] p-0 text-xs leading-none text-dim hover:bg-white/10 hover:text-foreground"
              >
                ×
              </button>
              {isActive && (
                <span aria-hidden className="absolute inset-x-0 -bottom-px h-0.5 bg-soft" />
              )}
            </div>
          )
        })}
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger
          title="New tab"
          aria-label="New tab"
          disabled={!connected || removing}
          className="w-[34px] shrink-0 cursor-pointer text-lg text-faint outline-none hover:text-foreground disabled:opacity-40"
        >
          +
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" sideOffset={4} className="w-[200px]" onCloseAutoFocus={restoreTerminalFocus}>
          <DropdownMenuItem onSelect={() => void newAgentTab(hostId, worktree)}>
            Claude Code<DropdownMenuShortcut>⌥C</DropdownMenuShortcut>
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
