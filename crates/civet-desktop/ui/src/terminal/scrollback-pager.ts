// Lazy scrollback paging (docs/ARCHITECTURE.md "Scrollback").
//
// xterm.js cannot prepend to its scrollback, so history older than what was
// replayed into the live terminal is fetched page by page with
// `fetch_scrollback { before: { end, limit } }` when the user scrolls to the
// top, and shown in a read-only history view above the live terminal. Lines
// skipped by coalescing (HISTORY_GAP) are fetched on demand with `range`.

import type { ScrollbackPageView, ScrollbackQuery } from '../types'
import type { LineRange } from './pane-stream'

export const SCROLLBACK_PAGE_LINES = 500
/** Server caps a Range fetch at whatever it retains; keep UI requests bounded too. */
export const MAX_GAP_FETCH_LINES = 5000

export type FetchScrollback = (query: ScrollbackQuery) => Promise<ScrollbackPageView>

export type HistoryLine = { line: number; vt: string; wrapped: boolean }

export class ScrollbackPager {
  /** Older history (contiguous, oldest first) loaded above the live terminal. */
  lines: HistoryLine[] = []
  /** Oldest line the daemon still retains (known after the first fetch). */
  oldestAvailable: number | null = null
  loading = false
  error: string | null = null
  private listeners = new Set<() => void>()

  constructor(
    private readonly fetch: FetchScrollback,
    private readonly pageLines = SCROLLBACK_PAGE_LINES
  ) {}

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn)
    return () => this.listeners.delete(fn)
  }

  /** Absolute number of the oldest loaded line, or null when nothing is loaded. */
  get oldestLoaded(): number | null {
    return this.lines[0]?.line ?? null
  }

  /** True when the daemon has nothing older than what we already show. */
  exhausted(oldestLocal: number): boolean {
    const oldest = this.oldestLoaded ?? oldestLocal
    return this.oldestAvailable !== null ? oldest <= this.oldestAvailable : oldest <= 0
  }

  /**
   * Fetch the page before the oldest line we have (`oldestLocal` = oldest line
   * still in xterm's scrollback when nothing has been paged yet).
   * Returns the number of lines added.
   */
  async loadOlder(oldestLocal: number): Promise<number> {
    if (this.loading || this.exhausted(oldestLocal)) return 0
    const end = this.oldestLoaded ?? oldestLocal
    this.loading = true
    this.error = null
    this.emit()
    try {
      const page = await this.fetch({ before: { end, limit: this.pageLines } })
      this.oldestAvailable = page.oldestAvailable
      const fresh = toLines(page).filter((l) => l.line < end)
      this.lines = [...fresh, ...this.lines]
      if (fresh.length === 0 && page.start >= end) {
        // Nothing older is retained (evicted): stop asking.
        this.oldestAvailable = end
      }
      return fresh.length
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err)
      return 0
    } finally {
      this.loading = false
      this.emit()
    }
  }

  /** Fetch lines skipped by coalescing. Bounded to the newest MAX_GAP_FETCH_LINES of the gap. */
  async loadGap(gap: LineRange): Promise<HistoryLine[]> {
    const start = Math.max(gap.start, gap.end - MAX_GAP_FETCH_LINES)
    const page = await this.fetch({ range: { start, end: gap.end } })
    return toLines(page)
  }

  reset(): void {
    this.lines = []
    this.oldestAvailable = null
    this.error = null
    this.emit()
  }

  private emit(): void {
    for (const fn of this.listeners) fn()
  }
}

function toLines(page: ScrollbackPageView): HistoryLine[] {
  return page.lines.map((vt, i) => ({
    line: page.start + i,
    vt,
    wrapped: page.wrapped[i] ?? false
  }))
}

/** Serialize history lines for a read-only xterm: wrapped rows continue without CRLF. */
export function historyToVt(lines: readonly HistoryLine[]): string {
  let out = ''
  lines.forEach((l, i) => {
    out += l.vt
    if (i < lines.length - 1 && !l.wrapped) out += '\r\n'
  })
  return out
}
