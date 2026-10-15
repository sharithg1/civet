import { describe, expect, it, vi } from 'vitest'
import { ScrollbackPager, historyToVt } from './scrollback-pager'
import type { ScrollbackPageView, ScrollbackQuery } from '../types'

function fakeServer(oldest: number, endAvailable: number) {
  return vi.fn(async (q: ScrollbackQuery): Promise<ScrollbackPageView> => {
    let start: number
    let end: number
    if ('before' in q) {
      end = Math.min(q.before.end ?? endAvailable, endAvailable)
      start = Math.max(oldest, end - q.before.limit)
    } else {
      start = Math.max(oldest, q.range.start)
      end = Math.min(endAvailable, q.range.end)
    }
    const lines = Array.from({ length: Math.max(0, end - start) }, (_, i) => `L${start + i}`)
    return { start, oldestAvailable: oldest, endAvailable, lines, wrapped: lines.map(() => false) }
  })
}

describe('ScrollbackPager', () => {
  it('pages upward from the oldest local line until the retained start', async () => {
    const fetch = fakeServer(100, 2000)
    const p = new ScrollbackPager(fetch, 300)
    expect(await p.loadOlder(1000)).toBe(300)
    expect(fetch).toHaveBeenLastCalledWith({ before: { end: 1000, limit: 300 } })
    expect(p.oldestLoaded).toBe(700)
    expect(p.lines.at(-1)!.line).toBe(999)
    expect(await p.loadOlder(1000)).toBe(300)
    expect(fetch).toHaveBeenLastCalledWith({ before: { end: 700, limit: 300 } })
    expect(await p.loadOlder(1000)).toBe(300) // 100..400
    expect(p.oldestLoaded).toBe(100)
    expect(p.exhausted(1000)).toBe(true)
    expect(await p.loadOlder(1000)).toBe(0)
    expect(fetch).toHaveBeenCalledTimes(3)
    // contiguous, oldest first
    expect(p.lines.map((l) => l.line)).toEqual(Array.from({ length: 900 }, (_, i) => 100 + i))
  })

  it('does nothing when the local scrollback already reaches line 0', async () => {
    const fetch = fakeServer(0, 50)
    const p = new ScrollbackPager(fetch)
    expect(await p.loadOlder(0)).toBe(0)
    expect(fetch).not.toHaveBeenCalled()
  })

  it('ignores concurrent loads and notifies subscribers', async () => {
    const fetch = fakeServer(0, 5000)
    const p = new ScrollbackPager(fetch, 10)
    const seen: boolean[] = []
    p.subscribe(() => seen.push(p.loading))
    const a = p.loadOlder(100)
    const b = p.loadOlder(100)
    expect(await b).toBe(0)
    expect(await a).toBe(10)
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(seen).toEqual([true, false])
  })

  it('records fetch errors without throwing', async () => {
    const p = new ScrollbackPager(async () => {
      throw new Error('connection lost')
    })
    expect(await p.loadOlder(10)).toBe(0)
    expect(p.error).toBe('connection lost')
    expect(p.loading).toBe(false)
  })

  it('fetches gaps with a bounded range', async () => {
    const fetch = fakeServer(0, 100_000)
    const p = new ScrollbackPager(fetch)
    const lines = await p.loadGap({ start: 10, end: 20 })
    expect(fetch).toHaveBeenLastCalledWith({ range: { start: 10, end: 20 } })
    expect(lines.map((l) => l.line)).toEqual([10, 11, 12, 13, 14, 15, 16, 17, 18, 19])
    await p.loadGap({ start: 0, end: 90_000 })
    expect(fetch).toHaveBeenLastCalledWith({ range: { start: 85_000, end: 90_000 } })
  })
})

describe('historyToVt', () => {
  it('joins lines with CRLF except after soft-wrapped rows', () => {
    expect(
      historyToVt([
        { line: 1, vt: 'a', wrapped: true },
        { line: 2, vt: 'b', wrapped: false },
        { line: 3, vt: 'c', wrapped: false }
      ])
    ).toBe('ab\r\nc')
  })
})
