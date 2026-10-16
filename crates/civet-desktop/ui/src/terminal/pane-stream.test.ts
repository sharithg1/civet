import { describe, expect, it, vi } from 'vitest'
import { PaneStream, type TerminalSink } from './pane-stream'
import { RecordFlag, RecordKind, encodeRecords, parseRecords, type RecordInit } from './records'

class FakeTerm implements TerminalSink {
  cols = 80
  rows = 24
  log: string[] = []
  resize(cols: number, rows: number) {
    this.cols = cols
    this.rows = rows
    this.log.push(`resize ${cols}x${rows}`)
  }
  write(data: Uint8Array) {
    this.log.push(`write ${new TextDecoder().decode(data)}`)
  }
}

const recs = (...r: RecordInit[]) => parseRecords(encodeRecords(r))

describe('PaneStream.apply', () => {
  it('resizes to the record size before writing its payload', () => {
    const term = new FakeTerm()
    const s = new PaneStream()
    s.apply(
      recs({
        cols: 100,
        rows: 30,
        state: 1,
        flags: RecordFlag.Full | RecordFlag.Resized,
        payload: 'A'
      }),
      term
    )
    expect(term.log).toEqual(['resize 100x30', 'write A'])
  })

  it('does not resize when the size already matches and skips empty payloads', () => {
    const term = new FakeTerm()
    const s = new PaneStream()
    s.apply(
      recs(
        { cols: 80, rows: 24, state: 2, payload: '' },
        { cols: 80, rows: 24, state: 3, payload: 'x' }
      ),
      term
    )
    expect(term.log).toEqual(['write x'])
    expect(s.state).toBe(3)
  })

  it('tracks input acks, predictions and pane close', () => {
    const term = new FakeTerm()
    const onClosed = vi.fn()
    const s = new PaneStream({ onClosed })
    s.apply(
      recs(
        { kind: RecordKind.Screen, cols: 80, rows: 24, state: 5, ackedInputSeq: 2, payload: 's' },
        { kind: RecordKind.InputAck, ackedInputSeq: 4 },
        { kind: RecordKind.Predict, cols: 80, rows: 24, state: 5, payload: 'p' },
        { kind: 99, payload: 'ignored' },
        { kind: RecordKind.PaneClosed },
        { kind: RecordKind.PaneClosed }
      ),
      term
    )
    expect(term.log).toEqual(['write s', 'write p'])
    expect(s.ackedInputSeq).toBe(4)
    expect(s.state).toBe(5) // predictions do not advance server state
    expect(s.closed).toBe(true)
    expect(onClosed).toHaveBeenCalledTimes(1)
  })

  it('tracks replayed history segments and gaps', () => {
    const term = new FakeTerm()
    const onGap = vi.fn()
    const s = new PaneStream({ onGap })
    // Full replays 1000..2000.
    s.apply(
      recs({
        cols: 80,
        rows: 24,
        flags: RecordFlag.Full,
        state: 1,
        historyStart: 1000,
        historyLines: 2000
      }),
      term
    )
    expect(s.segments).toEqual([{ start: 1000, end: 2000 }])
    // Contiguous diff extends the segment.
    s.apply(recs({ cols: 80, rows: 24, state: 2, historyStart: 2000, historyLines: 2050 }), term)
    expect(s.segments).toEqual([{ start: 1000, end: 2050 }])
    // Coalesced burst: 2050..5000 skipped, 5000..7000 replayed.
    s.apply(
      recs({
        cols: 80,
        rows: 24,
        state: 3,
        flags: RecordFlag.HistoryGap,
        historyStart: 5000,
        historyLines: 7000
      }),
      term
    )
    expect(s.gaps).toEqual([{ start: 2050, end: 5000 }])
    expect(onGap).toHaveBeenCalledWith({ start: 2050, end: 5000 })
    expect(s.segments).toEqual([
      { start: 1000, end: 2050 },
      { start: 5000, end: 7000 }
    ])
    expect(s.historyLines).toBe(7000)
    // A new Full resets everything (RIS cleared xterm's scrollback).
    s.apply(
      recs({
        cols: 80,
        rows: 24,
        flags: RecordFlag.Full,
        state: 9,
        historyStart: 6500,
        historyLines: 7500
      }),
      term
    )
    expect(s.segments).toEqual([{ start: 6500, end: 7500 }])
    expect(s.gaps).toEqual([])
  })

  it('computes the oldest line still in xterm scrollback', () => {
    const s = new PaneStream()
    const term = new FakeTerm()
    expect(s.oldestLocalLine(0)).toBe(0)
    s.apply(
      recs({
        cols: 80,
        rows: 24,
        flags: RecordFlag.Full,
        state: 1,
        historyStart: 1000,
        historyLines: 2000
      }),
      term
    )
    expect(s.oldestLocalLine(1000)).toBe(1000)
    expect(s.oldestLocalLine(5000)).toBe(1000) // more rows than replayed (screen rows pushed): clamp
    expect(s.oldestLocalLine(400)).toBe(1600) // xterm trimmed its oldest rows
    expect(s.oldestLocalLine(0)).toBe(2000)
    s.apply(
      recs({
        cols: 80,
        rows: 24,
        state: 2,
        flags: RecordFlag.HistoryGap,
        historyStart: 3000,
        historyLines: 3100
      }),
      term
    )
    // segments: 1000..2000 (1000 lines) + 3000..3100 (100 lines)
    expect(s.oldestLocalLine(1100)).toBe(1000)
    expect(s.oldestLocalLine(150)).toBe(1950)
    expect(s.oldestLocalLine(50)).toBe(3050)
  })
})
