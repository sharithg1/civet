// Applies parsed channel records to an xterm-like sink and keeps track of
// which absolute history lines ended up in xterm.js's own scrollback, so the
// lazy scrollback pager knows where "older" history starts
// (docs/INTERFACES.md §6.2, docs/ARCHITECTURE.md "Scrollback").

import { RecordKind, hasHistoryGap, isFull, type TermRecord } from './records'

/** The subset of xterm's Terminal used for record application (mockable in tests). */
export type TerminalSink = {
  readonly cols: number
  readonly rows: number
  resize(cols: number, rows: number): void
  write(data: Uint8Array): void
}

export type LineRange = { start: number; end: number }

export type PaneStreamCallbacks = {
  onClosed?: () => void
  onResized?: (cols: number, rows: number) => void
  onGap?: (gap: LineRange) => void
}

export class PaneStream {
  /** Latest server state applied (screen records only; predictions overlay it). */
  state = 0
  ackedInputSeq = 0
  /** Absolute count of history lines the server has (== one past newest). */
  historyLines = 0
  closed = false
  /** Contiguous ranges replayed into xterm's scrollback since the last Full, oldest first. */
  segments: LineRange[] = []
  /** Ranges skipped by coalescing (HISTORY_GAP); only reachable via fetch_scrollback. */
  gaps: LineRange[] = []

  constructor(private readonly callbacks: PaneStreamCallbacks = {}) {}

  apply(records: readonly TermRecord[], sink: TerminalSink): void {
    for (const r of records) {
      switch (r.kind) {
        case RecordKind.PaneClosed:
          if (!this.closed) {
            this.closed = true
            this.callbacks.onClosed?.()
          }
          break
        case RecordKind.InputAck:
          this.ackedInputSeq = Math.max(this.ackedInputSeq, r.ackedInputSeq)
          break
        case RecordKind.Screen:
        case RecordKind.Predict:
          this.applyScreen(r, sink)
          break
        default:
          // Unknown kinds are appended in later protocol versions; skip them.
          break
      }
    }
  }

  private applyScreen(r: TermRecord, sink: TerminalSink): void {
    // The server PTY size is authoritative: resize before writing.
    if (r.cols > 0 && r.rows > 0 && (r.cols !== sink.cols || r.rows !== sink.rows)) {
      sink.resize(r.cols, r.rows)
      this.callbacks.onResized?.(r.cols, r.rows)
    }
    if (r.payload.byteLength > 0) sink.write(r.payload)
    if (r.kind !== RecordKind.Screen) return

    this.state = r.state
    this.ackedInputSeq = Math.max(this.ackedInputSeq, r.ackedInputSeq)
    if (isFull(r)) {
      // Full starts with RIS: xterm's scrollback is cleared, then
      // historyStart..historyLines is replayed.
      this.segments =
        r.historyLines > r.historyStart ? [{ start: r.historyStart, end: r.historyLines }] : []
      this.gaps = []
    } else {
      if (hasHistoryGap(r) || r.historyStart > this.historyLines) {
        const gap = { start: this.historyLines, end: r.historyStart }
        if (gap.end > gap.start) {
          this.gaps.push(gap)
          this.callbacks.onGap?.(gap)
        }
        if (r.historyLines > r.historyStart) {
          this.segments.push({ start: r.historyStart, end: r.historyLines })
        }
      } else if (r.historyLines > this.historyLines) {
        const last = this.segments[this.segments.length - 1]
        if (last && last.end === r.historyStart) last.end = r.historyLines
        else this.segments.push({ start: r.historyStart, end: r.historyLines })
      }
    }
    this.historyLines = r.historyLines
  }

  /**
   * Absolute number of the oldest history line still in xterm's scrollback,
   * given how many scrollback rows xterm currently holds (`buffer.normal.baseY`).
   * xterm trims the oldest rows once its own scrollback limit is hit.
   */
  oldestLocalLine(scrollbackRows: number): number {
    const total = this.segments.reduce((n, s) => n + (s.end - s.start), 0)
    let drop = Math.max(0, total - Math.max(0, scrollbackRows))
    for (const s of this.segments) {
      const len = s.end - s.start
      if (drop < len) return s.start + drop
      drop -= len
    }
    return this.historyLines
  }
}
