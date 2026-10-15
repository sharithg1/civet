// Ported from Orca: src/renderer/src/components/tab-bar/SortableTab.tsx and
// tab-bar/drop-indicator.ts (tab classes, active indicator, hover-reveal close
// button, middle-click close). Drag-to-reorder (dnd-kit), rename, pinning,
// context menus and i18n were dropped. shadcn Tabs is not used: these tabs
// carry their own close buttons and only switch which split layout is mounted.

import { Columns2, Plus, Rows2, X } from 'lucide-react'
import { leaves } from '@/layout/pane-tree'
import { aggregateStatus, indicatorFor } from '@/lib/agent-status'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import {
  activateTab,
  closeTab,
  newTerminalTab,
  splitFocused,
  useStore,
  type Tab
} from '@/state/store'
import { AgentStateDot } from './AgentStateDot'
import { IconButton } from './IconButton'

/** Tab-bar controls hover to --muted, as before (overrides ghost's dark accent). */
const BAR_HOVER = 'hover:bg-muted dark:hover:bg-muted'

const ACTIVE_TAB_INDICATOR_CLASSES =
  'pointer-events-none absolute inset-x-0 bottom-0 h-[2px] bg-foreground z-20'

function getTabRootStateClasses(isActive: boolean): string {
  return isActive
    ? 'bg-[color-mix(in_srgb,var(--foreground)_6%,var(--card))] text-foreground'
    : 'bg-card text-muted-foreground hover:text-foreground'
}

type Props = {
  hostId: string
  worktree: string
  wsKey: string
  tabs: Tab[]
  activeTab: string | null
}

export function TabBar({ hostId, worktree, wsKey, tabs, activeTab }: Props) {
  return (
    <div className="flex h-9 shrink-0 items-stretch border-b border-border bg-card">
      <div className="flex min-w-0 flex-1 items-stretch overflow-x-auto" role="tablist">
        {tabs.map((tab, i) => (
          <TerminalTab
            key={tab.id}
            hostId={hostId}
            wsKey={wsKey}
            tab={tab}
            isActive={tab.id === activeTab}
            hasTabsToRight={i < tabs.length - 1}
          />
        ))}
        <IconButton
          label="New terminal"
          side="bottom"
          className={cn('h-full w-8 rounded-none', BAR_HOVER)}
          onClick={() => void newTerminalTab(hostId, worktree)}
        >
          <Plus className="size-4" />
        </IconButton>
      </div>
      <div className="flex items-center gap-0.5 px-1.5">
        <IconButton
          label="Split right"
          side="bottom"
          className={cn('rounded', BAR_HOVER)}
          onClick={() => void splitFocused('row')}
        >
          <Columns2 className="size-3.5" />
        </IconButton>
        <IconButton
          label="Split down"
          side="bottom"
          className={cn('rounded', BAR_HOVER)}
          onClick={() => void splitFocused('column')}
        >
          <Rows2 className="size-3.5" />
        </IconButton>
      </div>
    </div>
  )
}

function TerminalTab({
  hostId,
  wsKey,
  tab,
  isActive,
  hasTabsToRight
}: {
  hostId: string
  wsKey: string
  tab: Tab
  isActive: boolean
  hasTabsToRight: boolean
}) {
  const panes = useStore((s) => s.panes[hostId])
  const ids = leaves(tab.root)
  const infos = ids.map((id) => panes?.[id]).filter((p) => p !== undefined)
  const focusedInfo = panes?.[tab.focused]
  const title = focusedInfo?.title || focusedInfo?.command.join(' ') || `pane ${tab.focused}`
  const hasAgent = infos.some((p) => p.agent)
  const status = aggregateStatus(infos)
  return (
    <div
      role="tab"
      aria-selected={isActive}
      data-active={isActive ? 'true' : 'false'}
      className={`group relative flex h-full max-w-56 min-w-24 cursor-pointer select-none items-center px-2 text-xs outline-none ${
        hasTabsToRight ? 'border-r' : ''
      } border-border ${getTabRootStateClasses(isActive)}`}
      onClick={() => activateTab(wsKey, tab.id)}
      onMouseDown={(e) => {
        if (e.button === 1) e.preventDefault()
      }}
      onAuxClick={(e) => {
        if (e.button === 1) {
          e.preventDefault()
          closeTab(wsKey, tab.id)
        }
      }}
    >
      {isActive && <span className={ACTIVE_TAB_INDICATOR_CLASSES} aria-hidden />}
      {hasAgent && <AgentStateDot state={indicatorFor(status)} className="mr-1.5" />}
      <span className="mr-1 min-w-0 flex-1 truncate">{title}</span>
      {ids.length > 1 && <span className="mr-1 text-[10px] opacity-60">{ids.length}</span>}
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className={cn(
          'relative z-10 size-4 rounded-sm',
          BAR_HOVER,
          isActive
            ? 'text-muted-foreground hover:text-foreground'
            : 'text-transparent group-hover:text-muted-foreground hover:!text-foreground focus-visible:text-muted-foreground'
        )}
        aria-label={`Close tab ${title}`}
        onClick={(e) => {
          e.stopPropagation()
          closeTab(wsKey, tab.id)
        }}
      >
        <X className="size-3" />
      </Button>
    </div>
  )
}
