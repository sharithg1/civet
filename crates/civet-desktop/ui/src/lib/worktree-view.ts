// Pure view helpers for the v3 workspace chrome: status dots, labels, project
// tiles, relative times and the worktree switcher's fuzzy matcher. Everything
// here is derived client-side from what the daemon reports (WorktreeInfo,
// PaneInfo); see the v3 design's `wtSt`, `tabWhat`, `fz`, `match`, `hl`.

import type { PaneInfo, WorktreeInfo } from '../types'
import { basename } from './utils'

/** Status dot vocabulary of the design (plus `ok`: an unread "agent finished"). */
export type DotKind = 'run' | 'wait' | 'err' | 'ok' | 'idle' | 'disc'

export const DOT_LABEL: Record<DotKind, string> = {
  run: 'Running',
  wait: 'Waiting for you',
  err: 'Error',
  ok: 'Finished',
  idle: 'Idle',
  disc: 'Disconnected'
}

/** Status of one pane's agent (terminals are always idle). */
export function paneDot(p: PaneInfo | undefined, connected: boolean): DotKind {
  if (!connected) return 'disc'
  if (!p || !p.agent) return 'idle'
  if (p.exited && p.exitCode !== null && p.exitCode !== 0) return 'err'
  if (p.exited) return 'idle'
  if (p.status === 'needsInput') return 'wait'
  if (p.status === 'running') return 'run'
  return 'idle'
}

const PRIORITY: DotKind[] = ['wait', 'err', 'run', 'ok']

/**
 * Worktree dot: the most attention-worthy agent status among its panes. An
 * unread "done" (agent finished while you were elsewhere) shows as `ok`
 * until the worktree is visited.
 */
export function worktreeDot(
  panes: readonly PaneInfo[],
  connected: boolean,
  unread?: 'done' | 'needsInput' | string
): DotKind {
  if (!connected) return 'disc'
  const kinds = new Set(panes.map((p) => paneDot(p, true)))
  if (unread === 'done') kinds.add('ok')
  if (unread === 'needsInput') kinds.add('wait')
  return PRIORITY.find((k) => kinds.has(k)) ?? 'idle'
}

/** Split `NO-TICKET/short-task` into a dimmed prefix and the name. */
export function splitBranch(branch: string): { prefix: string; name: string } {
  const i = branch.lastIndexOf('/')
  return i < 0 ? { prefix: '', name: branch } : { prefix: branch.slice(0, i + 1), name: branch.slice(i + 1) }
}

export function worktreeBranch(wt: WorktreeInfo): string {
  return wt.branch ?? basename(wt.path)
}

/** Strip leading spinner / status glyphs that agents put in terminal titles ("✳ Fix …"). */
export function cleanTitle(title: string): string {
  return title.replace(/^[^\p{L}\p{N}]+/u, '').trim()
}

/** Titles agents show before they have named the session. */
const GENERIC_AGENT_TITLES = new Set(['claude code', 'claude', 'codex'])

/**
 * An agent pane's own title (Claude Code sets it to a summary of the session
 * via OSC 0/2), or null while it is only a placeholder ("✳ Claude Code") or
 * the command line.
 */
export function agentTitle(p: PaneInfo): string | null {
  const t = cleanTitle(p.title)
  const low = t.toLowerCase()
  if (!t || GENERIC_AGENT_TITLES.has(low)) return null
  if (low === (p.agent ?? '').toLowerCase() || low === p.command.join(' ').toLowerCase()) return null
  return t
}

/**
 * The session label from a submitted prompt: its first non-empty line,
 * collapsed whitespace, at most 80 characters. Slash commands ("/clear")
 * are not a task, so null.
 */
export function promptLabel(message: string | null | undefined): string | null {
  const line = (message ?? '')
    .split('\n')
    .map((l) => l.replace(/\s+/g, ' ').trim())
    .find((l) => l)
  if (!line || line.startsWith('/')) return null
  return line.length > 80 ? `${line.slice(0, 79).trimEnd()}…` : line
}

/**
 * The worktree's "task": the title of its oldest agent pane, else that
 * pane's first prompt, when it says more than the branch does.
 */
export function worktreeTask(wt: WorktreeInfo, panes: readonly PaneInfo[]): string | null {
  const agents = panes.filter((p) => p.agent).sort((a, b) => a.id - b.id)
  const branch = worktreeBranch(wt)
  for (const p of agents) {
    const t = agentTitle(p) ?? p.prompt
    if (!t) continue
    const low = t.toLowerCase()
    if (low === branch.toLowerCase() || low === splitBranch(branch).name.toLowerCase()) continue
    return t
  }
  return null
}

const capitalize = (s: string) => (s ? s[0]!.toUpperCase() + s.slice(1) : s)

/** "claude" → "Claude"; custom commands use their program name. */
export function agentName(p: Pick<PaneInfo, 'agent' | 'command'>): string {
  return capitalize(basename(p.agent ?? p.command[0] ?? 'agent'))
}

/**
 * Labels for a worktree's tabs as the design names them: "Claude 1",
 * "Claude 2", "Terminal", "Terminal 2", numbered by kind in tab order. A tab
 * is named after its first agent pane, else it is a terminal.
 */
export function tabLabels(tabPanes: readonly (readonly (PaneInfo | undefined)[])[]): {
  label: string
  agent: PaneInfo | null
}[] {
  const counts = new Map<string, number>()
  return tabPanes.map((panes) => {
    const agent = panes.find((p) => p?.agent) ?? null
    const kind = agent ? agentName(agent) : 'Terminal'
    const n = (counts.get(kind) ?? 0) + 1
    counts.set(kind, n)
    return { label: agent ? `${kind} ${n}` : n === 1 ? 'Terminal' : `Terminal ${n}`, agent }
  })
}

/** What a pane is doing, for the sidebar tab list and tooltips. */
export function paneWhat(p: PaneInfo | undefined): { text: string; mono: boolean } {
  if (!p) return { text: 'shell', mono: true }
  if (p.agent) return { text: agentTitle(p) ?? p.prompt ?? 'new session', mono: false }
  return { text: cleanTitle(p.title) || p.command.join(' ') || 'shell', mono: true }
}

/** `/home/me/x` or `/Users/me/x` → `~/x`. */
export function shortPath(path: string): string {
  return path.replace(/^\/(?:home|Users)\/[^/]+(?=\/|$)/, '~')
}

/** Compact age: now, 4m, 3h, 2d. */
export function ago(ms: number, now = Date.now()): string {
  const m = Math.round((now - ms) / 60000)
  if (m < 1) return 'now'
  if (m < 60) return `${m}m`
  if (m < 1440) return `${Math.round(m / 60)}h`
  return `${Math.round(m / 1440)}d`
}

/** Session age for the status bar: 58m, 3h 12m, 2d 4h. */
export function duration(ms: number, now = Date.now()): string {
  const m = Math.max(0, Math.floor((now - ms) / 60000))
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h ${m % 60}m`
  return `${Math.floor(h / 24)}d ${h % 24}h`
}

/** Stable per-project hue for the coloured initial tile. */
const TILE_HUES = [195, 300, 70, 20, 155, 250, 110, 340]
export function tileHue(name: string): number {
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0
  return TILE_HUES[h % TILE_HUES.length]!
}
export function tileColor(name: string): string {
  return `oklch(var(--tile-l) var(--tile-c) ${tileHue(name)})`
}

// ── Fuzzy matching (worktree switcher) ─────────────────────────────────────

export type FuzzyHit = { score: number; idxs: number[] }

/** Subsequence match; consecutive and word-start characters score higher. */
export function fuzzy(q: string, text: string): FuzzyHit | null {
  const t = text.toLowerCase()
  let i = 0
  let score = 0
  let last = -2
  const idxs: number[] = []
  for (const ch of q) {
    const j = t.indexOf(ch, i)
    if (j < 0) return null
    let sc = 1
    if (j === last + 1) sc += 3
    if (j === 0 || /[\s/\-_.]/.test(t[j - 1]!)) sc += 2
    score += sc
    idxs.push(j)
    last = j
    i = j + 1
  }
  return { score: score - t.length * 0.01, idxs }
}

/**
 * Every whitespace-separated token must match one of the fields; each token
 * goes to its best field. The first field (the primary label) gets a bonus.
 */
export function matchFields(
  tokens: readonly string[],
  fields: readonly string[]
): { score: number; hits: number[][] } | null {
  let total = 0
  const hits = fields.map(() => [] as number[])
  for (const tk of tokens) {
    let best: FuzzyHit | null = null
    let bi = -1
    fields.forEach((f, i) => {
      const r = fuzzy(tk, f)
      if (!r) return
      const sc = r.score + (i === 0 ? 0.5 : 0)
      if (!best || sc > best.score) {
        best = { score: sc, idxs: r.idxs }
        bi = i
      }
    })
    if (!best) return null
    total += (best as FuzzyHit).score
    hits[bi]!.push(...(best as FuzzyHit).idxs)
  }
  return { score: total, hits }
}

/** Split text into runs of matched / unmatched characters. */
export function highlightRuns(text: string, idxs: readonly number[]): { s: string; hit: boolean }[] {
  const set = new Set(idxs)
  const out: { s: string; hit: boolean }[] = []
  text.split('').forEach((ch, i) => {
    const hit = set.has(i)
    const cur = out[out.length - 1]
    if (cur && cur.hit === hit) cur.s += ch
    else out.push({ s: ch, hit })
  })
  return out
}
