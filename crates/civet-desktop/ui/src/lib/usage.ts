// Pure helpers for agent usage (state/usage.ts, the stats view's usage
// panel, the sidebar's per-worktree spend): rolling sessions up by local
// day, worktree and project, and formatting money and token counts. Costs
// are the daemon's estimates (micro-dollars at its list prices).

import type { AgentSessionUsage, RepoId, TokenCounts, WorktreeId } from '@/types'

export const ZERO_TOKENS: TokenCounts = { input: 0, output: 0, cacheWrite: 0, cacheRead: 0 }

export function totalTokens(t: TokenCounts): number {
  return t.input + t.output + t.cacheWrite + t.cacheRead
}

export function addTokens(a: TokenCounts, b: TokenCounts): TokenCounts {
  return {
    input: a.input + b.input,
    output: a.output + b.output,
    cacheWrite: a.cacheWrite + b.cacheWrite,
    cacheRead: a.cacheRead + b.cacheRead
  }
}

/** Micro-dollars → "$0.42", "$12.30", "$1,234", "<$0.01", "$0". */
export function formatUsd(micros: number): string {
  if (micros <= 0) return '$0'
  const usd = micros / 1e6
  if (usd < 0.01) return '<$0.01'
  if (usd >= 1000) return `$${Math.round(usd).toLocaleString('en-US')}`
  return `$${usd.toFixed(2)}`
}

/** 950, 12.3K, 4.5M, 1.2B. */
export function formatTokens(n: number): string {
  const units: [number, string][] = [
    [1e9, 'B'],
    [1e6, 'M'],
    [1e3, 'K']
  ]
  for (const [size, unit] of units)
    if (n >= size) {
      const v = n / size
      return `${v >= 100 ? Math.round(v) : v.toFixed(1)}${unit}`
    }
  return String(Math.round(n))
}

/** Local midnight of the day containing `ms`. */
export function startOfLocalDay(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** Totals of a set of sessions, counting only hours at or after `fromMs`. */
export type UsageTotals = {
  costMicros: number
  tokens: TokenCounts
  unpricedTokens: number
  /** Sessions with any usage in it. */
  sessions: number
  lastMs: number
}

const emptyTotals = (): UsageTotals => ({
  costMicros: 0,
  tokens: ZERO_TOKENS,
  unpricedTokens: 0,
  sessions: 0,
  lastMs: 0
})

function addSession(t: UsageTotals, s: AgentSessionUsage, fromMs: number): UsageTotals {
  let costMicros = 0
  let tokens = ZERO_TOKENS
  for (const h of s.hours)
    if (h.hourMs >= fromMs) {
      costMicros += h.costMicros
      tokens = addTokens(tokens, h.tokens)
    }
  const n = totalTokens(tokens)
  if (n === 0) return t
  // Unpriced tokens are per session: share them out by the hours counted.
  const all = totalTokens(s.tokens)
  const unpriced = all > 0 ? Math.round((s.unpricedTokens * n) / all) : 0
  return {
    costMicros: t.costMicros + costMicros,
    tokens: addTokens(t.tokens, tokens),
    unpricedTokens: t.unpricedTokens + unpriced,
    sessions: t.sessions + 1,
    lastMs: Math.max(t.lastMs, s.lastMs)
  }
}

export function totals(sessions: readonly AgentSessionUsage[], fromMs = 0): UsageTotals {
  return sessions.reduce((t, s) => addSession(t, s, fromMs), emptyTotals())
}

/** One row per key (worktree, project...), most spent first, then most tokens. */
export type UsageRow<K> = UsageTotals & { key: K }

function groupBy<K>(
  sessions: readonly AgentSessionUsage[],
  key: (s: AgentSessionUsage) => K,
  fromMs: number
): UsageRow<K>[] {
  const m = new Map<K, UsageTotals>()
  for (const s of sessions) {
    const k = key(s)
    m.set(k, addSession(m.get(k) ?? emptyTotals(), s, fromMs))
  }
  return [...m]
    .map(([k, t]) => ({ ...t, key: k }))
    .filter((r) => r.sessions > 0)
    .sort(
      (a, b) =>
        b.costMicros - a.costMicros ||
        totalTokens(b.tokens) - totalTokens(a.tokens) ||
        b.lastMs - a.lastMs
    )
}

/** By worktree; null collects sessions in no project's worktree. */
export function byWorktree(
  sessions: readonly AgentSessionUsage[],
  fromMs = 0
): UsageRow<WorktreeId | null>[] {
  return groupBy(sessions, (s) => s.worktree, fromMs)
}

/** By project; null collects sessions in no project. */
export function byProject(
  sessions: readonly AgentSessionUsage[],
  fromMs = 0
): UsageRow<RepoId | null>[] {
  return groupBy(sessions, (s) => s.repo, fromMs)
}

/** Spend and tokens of one local day. */
export type UsageDay = { dayMs: number; costMicros: number; tokens: number }

/**
 * The last `days` local days up to the one containing `nowMs`, oldest
 * first, days without usage included.
 */
export function dailyUsage(
  sessions: readonly AgentSessionUsage[],
  days: number,
  nowMs: number
): UsageDay[] {
  const today = startOfLocalDay(nowMs)
  const out: UsageDay[] = []
  // Step back by calendar days (DST days are 23 or 25 hours long).
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today)
    d.setDate(d.getDate() - i)
    out.push({ dayMs: d.getTime(), costMicros: 0, tokens: 0 })
  }
  const first = out[0]?.dayMs ?? today
  const tomorrow = new Date(today)
  tomorrow.setDate(tomorrow.getDate() + 1)
  for (const s of sessions)
    for (const h of s.hours) {
      if (h.hourMs < first || h.hourMs >= tomorrow.getTime()) continue
      const day = out.findLastIndex((d) => d.dayMs <= h.hourMs)
      out[day]!.costMicros += h.costMicros
      out[day]!.tokens += totalTokens(h.tokens)
    }
  return out
}

/** "claude-opus-5-5" → "Opus 5.5", "gpt-5-codex" → "gpt-5-codex". */
export function modelLabel(model: string): string {
  const m = /^claude-([a-z]+)-(\d+)(?:-(\d+))?/.exec(model)
  if (!m) return model
  const family = m[1]!.charAt(0).toUpperCase() + m[1]!.slice(1)
  return m[3] && m[3].length <= 2 ? `${family} ${m[2]}.${m[3]}` : `${family} ${m[2]}`
}
