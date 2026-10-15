// Ported from Orca: src/renderer/src/components/sidebar/WorktreeCard.tsx,
// worktree-card-header.tsx and worktree-card-surface.tsx (layout, tokens,
// status slot, unread emphasis). PR/issue/Jira/ports details, kanban, rename,
// lineage and the controller hooks were dropped: Civet's card shows the
// branch, path, pane count and the aggregated agent status.

import { GitBranch, Terminal } from 'lucide-react'
import { aggregateStatus, indicatorFor, statusLabel } from '../lib/agent-status'
import { basename, cn } from '../lib/utils'
import type { AgentStatus, PaneInfo, WorktreeInfo } from '../types'
import { AgentStateDot } from './AgentStateDot'

type Props = {
  worktree: WorktreeInfo
  panes: PaneInfo[]
  active: boolean
  unread: AgentStatus | undefined
  onSelect: () => void
}

export function WorktreeCard({ worktree, panes, active, unread, onSelect }: Props) {
  const live = panes.filter((p) => !p.exited)
  const status = aggregateStatus(panes)
  const hasAgent = panes.some((p) => p.agent)
  const name = worktree.branch ?? basename(worktree.path)
  return (
    <button
      type="button"
      onClick={onSelect}
      data-active={active ? 'true' : 'false'}
      className={cn(
        'group flex w-full min-w-0 flex-col gap-0.5 rounded-md px-2 py-1.5 text-left text-xs transition-colors',
        active
          ? 'bg-worktree-sidebar-accent text-worktree-sidebar-accent-foreground'
          : 'text-worktree-sidebar-foreground/85 hover:bg-worktree-sidebar-accent/60'
      )}
    >
      <div className="flex min-w-0 items-center gap-1.5">
        <span className="inline-flex h-3 w-3 shrink-0 items-center justify-center">
          {hasAgent ? (
            <AgentStateDot state={indicatorFor(status)} size="md" title={statusLabel(status)} />
          ) : (
            <GitBranch className="size-3 text-muted-foreground" aria-hidden />
          )}
        </span>
        <span className={cn('min-w-0 flex-1 truncate', unread ? 'font-semibold' : 'font-medium')}>
          {name}
        </span>
        {worktree.isMain && (
          <span className="rounded border border-worktree-sidebar-border px-1 text-[10px] text-muted-foreground">
            main
          </span>
        )}
        {unread && (
          <span
            className={cn(
              'size-1.5 shrink-0 rounded-full',
              unread === 'needsInput' ? 'bg-agent-question' : 'bg-emerald-500'
            )}
            aria-label={unread === 'needsInput' ? 'Agent needs input' : 'Agent finished'}
          />
        )}
      </div>
      <div className="flex min-w-0 items-center gap-1.5 pl-[18px] text-[11px] text-muted-foreground">
        <span className="min-w-0 flex-1 truncate" title={worktree.path}>
          {worktree.path}
        </span>
        {live.length > 0 && (
          <span className="flex shrink-0 items-center gap-0.5" title={`${live.length} live pane(s)`}>
            <Terminal className="size-3" aria-hidden />
            {live.length}
          </span>
        )}
      </div>
    </button>
  )
}
