// Maps the daemon's AgentStatus onto the sidebar/tab indicator vocabulary
// ported from Orca (StatusIndicator / AgentStateDot).
import type { AgentStatus, PaneInfo } from '../types'

export type IndicatorStatus = 'working' | 'permission' | 'done' | 'active' | 'inactive'

export function indicatorFor(status: AgentStatus): IndicatorStatus {
  switch (status) {
    case 'running':
      return 'working'
    case 'needsInput':
      return 'permission'
    case 'done':
      return 'done'
    case 'idle':
      return 'active'
    default:
      return 'inactive'
  }
}

const PRIORITY: Record<AgentStatus, number> = {
  needsInput: 4,
  running: 3,
  done: 2,
  idle: 1,
  unknown: 0
}

/** Worktree badge: the most attention-worthy status among its panes. */
export function aggregateStatus(panes: readonly Pick<PaneInfo, 'status'>[]): AgentStatus {
  let best: AgentStatus = 'unknown'
  for (const p of panes) if (PRIORITY[p.status] > PRIORITY[best]) best = p.status
  return best
}

export function statusLabel(status: AgentStatus): string {
  switch (status) {
    case 'running':
      return 'Running'
    case 'needsInput':
      return 'Needs input'
    case 'done':
      return 'Done'
    case 'idle':
      return 'Idle'
    default:
      return 'No agent activity'
  }
}
