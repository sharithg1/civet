import { describe, expect, it } from 'vitest'
import type { AgentSessionUsage, UsageHour } from '@/types'
import {
  byProject,
  byWorktree,
  dailyUsage,
  formatTokens,
  formatUsd,
  modelLabel,
  startOfLocalDay,
  totals,
  totalTokens
} from './usage'

const HOUR = 3_600_000

function hour(hourMs: number, costMicros: number, output = 100): UsageHour {
  return { hourMs, costMicros, tokens: { input: 1, output, cacheWrite: 0, cacheRead: 0 } }
}

function session(over: Partial<AgentSessionUsage> & { hours: UsageHour[] }): AgentSessionUsage {
  const tokens = over.hours.reduce(
    (t, h) => ({
      input: t.input + h.tokens.input,
      output: t.output + h.tokens.output,
      cacheWrite: t.cacheWrite + h.tokens.cacheWrite,
      cacheRead: t.cacheRead + h.tokens.cacheRead
    }),
    { input: 0, output: 0, cacheWrite: 0, cacheRead: 0 }
  )
  return {
    agent: 'claude',
    sessionId: 's',
    cwd: '/w',
    worktree: '/w',
    repo: 'r',
    models: ['claude-opus-5-5'],
    firstMs: over.hours[0]?.hourMs ?? 0,
    lastMs: over.hours.at(-1)?.hourMs ?? 0,
    tokens,
    costMicros: over.hours.reduce((n, h) => n + h.costMicros, 0),
    unpricedTokens: 0,
    ...over
  }
}

describe('formatting', () => {
  it('formats dollars from micro-dollars', () => {
    expect(formatUsd(0)).toBe('$0')
    expect(formatUsd(4_000)).toBe('<$0.01')
    expect(formatUsd(420_000)).toBe('$0.42')
    expect(formatUsd(12_300_000)).toBe('$12.30')
    expect(formatUsd(1_234_567_890)).toBe('$1,235')
  })

  it('formats token counts', () => {
    expect(formatTokens(950)).toBe('950')
    expect(formatTokens(12_345)).toBe('12.3K')
    expect(formatTokens(4_500_000)).toBe('4.5M')
    expect(formatTokens(298_472_864)).toBe('298M')
    expect(formatTokens(1.2e9)).toBe('1.2B')
  })

  it('names Claude models', () => {
    expect(modelLabel('claude-opus-5-5')).toBe('Opus 5.5')
    expect(modelLabel('claude-sonnet-5')).toBe('Sonnet 5')
    expect(modelLabel('claude-sonnet-4-5-20250929')).toBe('Sonnet 4.5')
    expect(modelLabel('claude-opus-4-20250514')).toBe('Opus 4')
    expect(modelLabel('gpt-5-codex')).toBe('gpt-5-codex')
  })
})

describe('rollups', () => {
  const now = new Date(2026, 9, 27, 15, 30).getTime()
  const today = startOfLocalDay(now)
  const a = session({
    sessionId: 'a',
    hours: [hour(today - 2 * 24 * HOUR, 1_000_000), hour(today + 10 * HOUR, 2_000_000)]
  })
  const b = session({
    sessionId: 'b',
    worktree: '/w2',
    hours: [hour(today + 11 * HOUR, 500_000)]
  })
  const c = session({
    sessionId: 'c',
    worktree: null,
    repo: null,
    agent: 'codex',
    models: ['gpt-6-astra'],
    hours: [hour(today + 12 * HOUR, 0, 999)],
    unpricedTokens: 1000
  })

  it('totals from a time', () => {
    const all = totals([a, b, c])
    expect(all.costMicros).toBe(3_500_000)
    expect(all.sessions).toBe(3)
    expect(all.unpricedTokens).toBe(1000)
    const t = totals([a, b, c], today)
    expect(t.costMicros).toBe(2_500_000)
    expect(totalTokens(t.tokens)).toBe(101 + 101 + 1000)
    // Only part of a's tokens count: nothing unpriced from it.
    expect(t.unpricedTokens).toBe(1000)
    expect(totals([a], today + 20 * HOUR).sessions).toBe(0)
  })

  it('groups by worktree and project, most spent first', () => {
    expect(byWorktree([a, b, c]).map((r) => [r.key, r.costMicros])).toEqual([
      ['/w', 3_000_000],
      ['/w2', 500_000],
      [null, 0]
    ])
    expect(byProject([a, b, c]).map((r) => [r.key, r.costMicros, r.sessions])).toEqual([
      ['r', 3_500_000, 2],
      [null, 0, 1]
    ])
    expect(byWorktree([a, b], today + 11 * HOUR).map((r) => r.key)).toEqual(['/w2'])
  })

  it('buckets hours into local days, empty days included', () => {
    const days = dailyUsage([a, b, c], 7, now)
    expect(days).toHaveLength(7)
    expect(days.at(-1)!.dayMs).toBe(today)
    expect(days.at(-1)!.costMicros).toBe(2_500_000)
    expect(days.at(-3)!.costMicros).toBe(1_000_000)
    expect(days.at(-2)!.costMicros).toBe(0)
    expect(days.at(-1)!.tokens).toBe(101 + 101 + 1000)
    // Older than the window: left out.
    expect(dailyUsage([a], 2, now).reduce((n, d) => n + d.costMicros, 0)).toBe(2_000_000)
  })
})
