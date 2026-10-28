// Broadcast and queued prompts (civetd >= 0.4.4).
//
// Broadcast: the prompt dialog sends one text to several agent panes, now
// (`send_prompt` per pane) or queued on each. Targets come from the dialog's
// checkboxes, preselected from the sidebar (⌘-click rows to select several
// worktrees) or a row's context menu.
//
// Queued prompts live on the daemon, which sends each one when its pane's
// agent is next done, so they fire while this app is closed. Here is a
// mirror for the sidebar: listed on connect, then kept current by
// `promptQueue` daemon events (a pane's whole queue each time). A prompt the
// daemon just sent gets a toast.

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import {
  agentPanesOf,
  broadcastSummary,
  promptPreview,
  refKey,
  type BroadcastOutcome,
  type PaneRef,
  type PromptWhen
} from '@/lib/prompts'
import type { DaemonEvent, PaneId, PromptTrigger, QueuedPrompt } from '@/types'
import { versionAtLeast } from './fileops'
import { pushToast, reportError, useStore, type HostView } from './store'

/** Oldest civetd that takes prompts (civet_proto::PROMPTS_MIN_DAEMON_VERSION). */
export const PROMPTS_MIN_DAEMON_VERSION = '0.4.4'

/** What the prompt dialog is doing. */
export type PromptDialog =
  /** Send (or queue) a new prompt; `targets` start checked. */
  | { kind: 'send'; targets: PaneRef[]; when?: PromptWhen }
  /** Edit (or cancel) a queued prompt. */
  | { kind: 'edit'; hostId: string; prompt: QueuedPrompt }

type PromptsState = {
  /** hostId → pane → its queued prompts, oldest (next) first */
  queues: Record<string, Record<PaneId, QueuedPrompt[]>>
  /** wsKeys of the worktrees ⌘-clicked in the sidebar (broadcast targets) */
  selected: string[]
  dialog: PromptDialog | null
}

export const usePrompts = create<PromptsState>(() => ({ queues: {}, selected: [], dialog: null }))

const set = usePrompts.setState

export function hasPrompts(h: HostView | undefined): boolean {
  return (
    h?.state === 'connected' &&
    (!h.daemonVersion || versionAtLeast(h.daemonVersion, PROMPTS_MIN_DAEMON_VERSION))
  )
}

let backend: CivetBackend | null = null

function b(): CivetBackend {
  if (!backend) throw new Error('prompts not initialised')
  return backend
}

/**
 * hostId → count of single-pane queue changes (events, local updates). A
 * host list that resolves after one of them is older than it, and is
 * fetched again rather than applied over it.
 */
const revisions = new Map<string, number>()

function setQueue(hostId: string, pane: PaneId, prompts: QueuedPrompt[]): void {
  revisions.set(hostId, (revisions.get(hostId) ?? 0) + 1)
  set((s) => {
    const host = { ...s.queues[hostId] }
    if (prompts.length) host[pane] = prompts
    else delete host[pane]
    return { queues: { ...s.queues, [hostId]: host } }
  })
}

function groupByPane(prompts: QueuedPrompt[]): Record<PaneId, QueuedPrompt[]> {
  const host: Record<PaneId, QueuedPrompt[]> = {}
  for (const p of prompts) (host[p.pane] ??= []).push(p)
  return host
}

function setHostQueues(hostId: string, prompts: QueuedPrompt[]): void {
  set((s) => ({ queues: { ...s.queues, [hostId]: groupByPane(prompts) } }))
}

/** Like setHostQueues, but panes already known here keep their queue. */
function mergeHostQueues(hostId: string, prompts: QueuedPrompt[]): void {
  set((s) => ({
    queues: { ...s.queues, [hostId]: { ...groupByPane(prompts), ...s.queues[hostId] } }
  }))
}

/** A daemon event (also used by tests). */
export function onPromptEvent(hostId: string, event: DaemonEvent): void {
  if (typeof event !== 'object') return
  if ('promptQueue' in event) {
    const { pane, prompts, sent } = event.promptQueue
    const before = usePrompts.getState().queues[hostId]?.[pane] ?? []
    setQueue(hostId, pane, prompts)
    const fired = sent != null ? before.find((p) => p.id === sent) : undefined
    if (sent != null) {
      const info = useStore.getState().panes[hostId]?.[pane]
      pushToast(
        {
          kind: 'info',
          title: `Queued prompt sent${info ? ` to ${info.title || `pane ${pane}`}` : ''}`,
          body: fired ? promptPreview(fired.text) : undefined,
          target: { hostId, worktree: info?.worktree ?? null, pane }
        },
        5000
      )
    }
  } else if ('paneClosed' in event) {
    setQueue(hostId, event.paneClosed.pane, [])
  }
}

/** Listen for queue changes and load each host's queues when it connects. */
export async function initPrompts(be: CivetBackend): Promise<() => void> {
  backend = be
  const unlisten = await be.onDaemonEvent(({ hostId, event }) => onPromptEvent(hostId, event))
  const loaded = new Set<string>()
  const sync = (hosts: Record<string, HostView>) => {
    for (const [id, h] of Object.entries(hosts)) {
      if (hasPrompts(h) && !loaded.has(id)) {
        loaded.add(id)
        void loadHost(id)
      } else if (h.state !== 'connected') {
        // Reload on the way back (the daemon may have sent some meanwhile,
        // or be a new instance with no queues).
        loaded.delete(id)
      }
    }
    for (const id of [...loaded]) {
      if (!hosts[id]) {
        loaded.delete(id)
        set((s) => {
          const queues = { ...s.queues }
          delete queues[id]
          return { queues }
        })
      }
    }
  }
  sync(useStore.getState().hosts)
  const unsub = useStore.subscribe((s, prev) => {
    if (s.hosts !== prev.hosts) sync(s.hosts)
  })
  return () => {
    unsub()
    unlisten()
  }
}

/** Lists retried when a queue change lands while one is in flight. */
const LOAD_ATTEMPTS = 3

async function loadHost(hostId: string): Promise<void> {
  try {
    for (let i = 1; ; i++) {
      const rev = revisions.get(hostId) ?? 0
      const list = await b().listQueuedPrompts(hostId)
      if (revisions.get(hostId) === rev) return setHostQueues(hostId, list)
      // Still changing: the panes it changed are newer here; take the rest.
      if (i === LOAD_ATTEMPTS) return mergeHostQueues(hostId, list)
    }
  } catch (err) {
    console.warn('[civet] queued prompts', hostId, err)
  }
}

// ── Sidebar selection ─────────────────────────────────────────────────────

export function toggleSelected(key: string): void {
  set((s) => ({
    selected: s.selected.includes(key) ? s.selected.filter((k) => k !== key) : [...s.selected, key]
  }))
}

export function clearSelected(): void {
  if (usePrompts.getState().selected.length) set({ selected: [] })
}

// ── Dialog ────────────────────────────────────────────────────────────────

export function openPromptDialog(dialog: PromptDialog): void {
  set({ dialog })
}

/** The live agent panes of worktrees (wsKeys), as dialog targets. */
export function agentTargets(keys: readonly string[]): PaneRef[] {
  const panes = useStore.getState().panes
  return keys.flatMap((key) => {
    const [hostId, worktree] = key.split('\u0000') as [string, string]
    return agentPanesOf(panes[hostId], worktree).map((p) => ({ hostId, pane: p.id }))
  })
}

/**
 * Open "Send prompt" with the sidebar selection checked, else `keys`
 * (a row's context menu), else the active worktree's agents.
 */
export function openSendPrompt(keys?: readonly string[]): void {
  const { selected } = usePrompts.getState()
  const active = useStore.getState().active
  const from = selected.length
    ? selected
    : (keys ?? (active ? [`${active.hostId}\u0000${active.worktree}`] : []))
  openPromptDialog({ kind: 'send', targets: agentTargets(from) })
}

export function closePromptDialog(): void {
  set({ dialog: null })
}

// ── Actions ───────────────────────────────────────────────────────────────

/**
 * Send `text` to every target now, or queue it on each with `when` as the
 * trigger. Per-pane failures (an old daemon, a pane that exited) do not
 * stop the others; one toast sums it up. Resolves to the outcome.
 */
export async function broadcastPrompt(
  targets: readonly PaneRef[],
  text: string,
  when: PromptWhen
): Promise<BroadcastOutcome> {
  const unique = [...new Map(targets.map((t) => [refKey(t), t])).values()]
  const results = await Promise.allSettled(
    unique.map(async (t) => {
      if (when === 'now') return b().sendPrompt(t.hostId, t.pane, text)
      const p = await b().queuePrompt(t.hostId, t.pane, text, when)
      // The promptQueue event follows; show it right away regardless.
      upsertQueued(t.hostId, p)
    })
  )
  const outcome: BroadcastOutcome = { ok: 0, failed: [] }
  results.forEach((r, i) => {
    if (r.status === 'fulfilled') outcome.ok++
    else outcome.failed.push({ ref: unique[i]!, message: CivetError.from(r.reason).message })
  })
  pushToast(broadcastSummary(when, outcome), outcome.failed.length ? 9000 : 3000)
  return outcome
}

function upsertQueued(hostId: string, p: QueuedPrompt): void {
  const cur = usePrompts.getState().queues[hostId]?.[p.pane] ?? []
  const i = cur.findIndex((q) => q.id === p.id)
  setQueue(hostId, p.pane, i < 0 ? [...cur, p] : cur.map((q) => (q.id === p.id ? p : q)))
}

/**
 * Queue `text` again on its pane (a "Send now" that took it off the queue
 * and then failed), so closing the dialog loses nothing. Null (after an
 * error toast) if that fails too.
 */
export async function requeuePrompt(
  hostId: string,
  pane: PaneId,
  text: string,
  trigger: PromptTrigger
): Promise<QueuedPrompt | null> {
  try {
    const p = await b().queuePrompt(hostId, pane, text, trigger)
    upsertQueued(hostId, p)
    return p
  } catch (err) {
    reportError('Could not put the prompt back in the queue', err)
    return null
  }
}

export async function editQueuedPrompt(
  hostId: string,
  id: number,
  text: string,
  trigger: PromptTrigger
): Promise<void> {
  try {
    upsertQueued(hostId, await b().editQueuedPrompt(hostId, id, text, trigger))
  } catch (err) {
    reportError('Could not change the queued prompt', err)
    throw err
  }
}

/** Resolves to false (after an error toast) if it could not be cancelled, e.g. it was just sent. */
export async function cancelQueuedPrompt(hostId: string, prompt: QueuedPrompt): Promise<boolean> {
  try {
    await b().cancelQueuedPrompt(hostId, prompt.id)
    const cur = usePrompts.getState().queues[hostId]?.[prompt.pane] ?? []
    setQueue(
      hostId,
      prompt.pane,
      cur.filter((q) => q.id !== prompt.id)
    )
    return true
  } catch (err) {
    reportError('Could not cancel the queued prompt', err)
    return false
  }
}
