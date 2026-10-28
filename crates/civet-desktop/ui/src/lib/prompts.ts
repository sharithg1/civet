// Pure helpers for broadcast and queued prompts (state/prompts.ts, the
// prompt dialog and the sidebar's queued-prompt rows): which panes a prompt
// can go to, how the dialog's "When" maps to a daemon trigger, and how a
// broadcast's per-pane outcomes read as one toast.

import type { PaneId, PaneInfo, PromptTrigger, WorktreeInfo } from '../types'
import { agentName, agentTitle } from './worktree-view'

/** One agent pane a prompt can go to. */
export type PaneRef = { hostId: string; pane: PaneId }

/** The dialog's "When": now (broadcast) or queued with a trigger. */
export type PromptWhen = 'now' | PromptTrigger

export const WHEN_LABEL: Record<PromptWhen, string> = {
  now: 'Now',
  done: 'When done',
  doneOrNeedsInput: 'When done or needs input'
}

/** Short trigger text for a queued prompt's row. */
export function triggerLabel(trigger: PromptTrigger): string {
  return trigger === 'done' ? 'when done' : 'when done or needs input'
}

export const refKey = (r: PaneRef) => `${r.hostId}\u0000${r.pane}`

/** A live agent pane: prompts make sense there (not in plain terminals). */
export function isPromptTarget(p: PaneInfo | undefined): p is PaneInfo {
  return !!p?.agent && !p.exited
}

/** A worktree's live agent panes, oldest first. */
export function agentPanesOf(
  panes: Record<PaneId, PaneInfo> | undefined,
  worktree: string
): PaneInfo[] {
  return Object.values(panes ?? {})
    .filter((p) => p.worktree === worktree && isPromptTarget(p))
    .sort((a, b) => a.id - b.id)
}

/** "Claude · Fix the flaky test" for a target row. */
export function paneTargetLabel(p: PaneInfo): string {
  const what = agentTitle(p) ?? p.prompt
  return what ? `${agentName(p)} · ${what}` : agentName(p)
}

export type TargetGroup = {
  hostId: string
  worktree: WorktreeInfo
  /** "project · branch" (host appended when there are several hosts). */
  label: string
  panes: PaneInfo[]
}

/**
 * Every worktree with live agent panes, as the dialog lists them: in the
 * order given (sidebar order), worktrees without agents left out.
 */
export function targetGroups(
  worktrees: readonly { hostId: string; wt: WorktreeInfo; project: string; host: string }[],
  panes: Record<string, Record<PaneId, PaneInfo>>,
  multiHost: boolean
): TargetGroup[] {
  const out: TargetGroup[] = []
  for (const { hostId, wt, project, host } of worktrees) {
    const agents = agentPanesOf(panes[hostId], wt.id)
    if (!agents.length) continue
    const branch = wt.branch ?? wt.path.slice(wt.path.lastIndexOf('/') + 1)
    out.push({
      hostId,
      worktree: wt,
      label: `${project} · ${branch}${multiHost ? ` · ${host}` : ''}`,
      panes: agents
    })
  }
  return out
}

/** Outcome of sending (or queueing) one prompt to several panes. */
export type BroadcastOutcome = { ok: number; failed: { ref: PaneRef; message: string }[] }

/** Toast text for a broadcast's outcome. */
export function broadcastSummary(
  when: PromptWhen,
  o: BroadcastOutcome
): { kind: 'info' | 'error'; title: string; body?: string } {
  const total = o.ok + o.failed.length
  const agents = (n: number) => `${n} agent${n === 1 ? '' : 's'}`
  const verb = when === 'now' ? 'Sent to' : 'Queued for'
  if (!o.failed.length) return { kind: 'info', title: `${verb} ${agents(o.ok)}` }
  const first = o.failed[0]!.message
  return {
    kind: 'error',
    title: o.ok
      ? `${verb} ${o.ok} of ${agents(total)}`
      : `Could not ${when === 'now' ? 'send to' : 'queue for'} ${agents(total)}`,
    body: o.failed.length > 1 ? `${first} (and ${o.failed.length - 1} more)` : first
  }
}

/** First line of a prompt, collapsed, for one-line rows and toasts. */
export function promptPreview(text: string, max = 80): string {
  const line =
    text
      .split('\n')
      .map((l) => l.replace(/\s+/g, ' ').trim())
      .find((l) => l) ?? ''
  const more = text.trim().includes('\n') ? ' …' : ''
  return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : `${line}${more}`
}
