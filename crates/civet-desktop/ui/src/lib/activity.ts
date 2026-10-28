// Pure view helpers for the Activity tab (right panel): what the agents in a
// worktree did, turn by turn, as civetd records it (`agent_activity`).

import type { AgentStatus, AgentTurn } from '../types'
import type { DotKind } from './worktree-view'

/** How a turn ended (or that it still runs), for its row. */
export const TURN_STATUS: Record<AgentStatus, [label: string, dot: DotKind]> = {
  running: ['Running', 'run'],
  needsInput: ['Needs input', 'wait'],
  done: ['Done', 'ok'],
  idle: ['Stopped', 'idle'],
  unknown: ['Interrupted', 'err']
}

/** Length of a span: 45s, 3m 12s, 1h 4m. */
export function spanLength(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ${s % 60}s`
  return `${Math.floor(m / 60)}h ${m % 60}m`
}

/** How long the turn ran (so far, while running). */
export function turnLength(turn: AgentTurn, now = Date.now()): string {
  return spanLength((turn.endedMs ?? now) - turn.startedMs)
}

const clock = (ms: number) =>
  new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

/** `14:02–14:20`, or `14:02–now` while running. */
export function turnTimes(turn: AgentTurn): string {
  return `${clock(turn.startedMs)}–${turn.endedMs == null ? 'now' : clock(turn.endedMs)}`
}

/** Local midnight before `ms`. */
function dayStart(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** `Today`, `Yesterday`, else e.g. `Mon, Oct 26`. */
export function dayLabel(ms: number, now = Date.now()): string {
  const days = Math.round((dayStart(now) - dayStart(ms)) / 86_400_000)
  if (days === 0) return 'Today'
  if (days === 1) return 'Yesterday'
  return new Date(ms).toLocaleDateString([], { weekday: 'short', month: 'short', day: 'numeric' })
}

/** Turns newest first, grouped by the day they started. */
export function turnsByDay(
  turns: readonly AgentTurn[],
  now = Date.now()
): { day: string; turns: AgentTurn[] }[] {
  const groups: { day: string; turns: AgentTurn[] }[] = []
  for (const t of [...turns].sort((a, b) => b.startedMs - a.startedMs || b.id - a.id)) {
    const day = dayLabel(t.startedMs, now)
    const last = groups[groups.length - 1]
    if (last?.day === day) last.turns.push(t)
    else groups.push({ day, turns: [t] })
  }
  return groups
}

/** Added / deleted lines over a turn's files (binary files count as none). */
export function turnLines(turn: AgentTurn): { additions: number; deletions: number } {
  let additions = 0
  let deletions = 0
  for (const f of turn.files) {
    additions += f.additions ?? 0
    deletions += f.deletions ?? 0
  }
  return { additions, deletions }
}
