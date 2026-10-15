import { describe, expect, it } from 'vitest'
import {
  RECORD_HEADER_LEN,
  RecordFlag,
  RecordKind,
  RecordParseError,
  encodeRecords,
  hasHistoryGap,
  isFull,
  isResized,
  parseRecords,
  toBytes
} from './records'

const text = (b: Uint8Array) => new TextDecoder().decode(b)

/** Build one record by hand, byte by byte, to pin the documented layout. */
function handRecord(): Uint8Array {
  const payload = new TextEncoder().encode('hi')
  const buf = new Uint8Array(RECORD_HEADER_LEN + payload.length)
  const v = new DataView(buf.buffer)
  v.setUint8(0, 1) // kind = screen
  v.setUint8(1, 0b011) // FULL | RESIZED
  v.setUint16(2, 132, true) // cols
  v.setUint16(4, 43, true) // rows
  v.setUint16(6, 0, true)
  v.setUint32(8, 7, true) // state lo
  v.setUint32(12, 1, true) // state hi → 2^32 + 7
  v.setUint32(16, 5, true) // ackedInputSeq
  v.setUint32(24, 1000, true) // historyStart
  v.setUint32(32, 2000, true) // historyLines
  v.setUint32(40, payload.length, true)
  buf.set(payload, 44)
  return buf
}

describe('parseRecords', () => {
  it('decodes the documented 44-byte little-endian header', () => {
    const [r] = parseRecords(handRecord())
    expect(r).toBeDefined()
    expect(r!.kind).toBe(RecordKind.Screen)
    expect(isFull(r!)).toBe(true)
    expect(isResized(r!)).toBe(true)
    expect(hasHistoryGap(r!)).toBe(false)
    expect(r!.cols).toBe(132)
    expect(r!.rows).toBe(43)
    expect(r!.state).toBe(2 ** 32 + 7)
    expect(r!.ackedInputSeq).toBe(5)
    expect(r!.historyStart).toBe(1000)
    expect(r!.historyLines).toBe(2000)
    expect(text(r!.payload)).toBe('hi')
  })

  it('parses several records back to back and returns zero-copy payload views', () => {
    const msg = encodeRecords([
      { kind: RecordKind.Screen, cols: 80, rows: 24, state: 1, payload: 'abc' },
      { kind: RecordKind.InputAck, ackedInputSeq: 9 },
      { kind: RecordKind.Screen, flags: RecordFlag.HistoryGap, state: 2, payload: '\x1b[1mX' },
      { kind: RecordKind.PaneClosed }
    ])
    const recs = parseRecords(msg.buffer as ArrayBuffer)
    expect(recs.map((r) => r.kind)).toEqual([1, 2, 1, 3])
    expect(text(recs[0]!.payload)).toBe('abc')
    expect(recs[1]!.ackedInputSeq).toBe(9)
    expect(recs[1]!.payload.byteLength).toBe(0)
    expect(hasHistoryGap(recs[2]!)).toBe(true)
    expect(text(recs[2]!.payload)).toBe('\x1b[1mX')
    // Same underlying buffer: no copy.
    expect(recs[0]!.payload.buffer).toBe(msg.buffer)
  })

  it('handles a Uint8Array view with a non-zero byteOffset', () => {
    const inner = encodeRecords([{ state: 3, cols: 10, rows: 5, payload: 'ok' }])
    const outer = new Uint8Array(inner.length + 8)
    outer.set(inner, 8)
    const [r] = parseRecords(outer.subarray(8))
    expect(r!.state).toBe(3)
    expect(text(r!.payload)).toBe('ok')
  })

  it('round-trips large u64 values below 2^53', () => {
    const big = Number.MAX_SAFE_INTEGER
    const [r] = parseRecords(encodeRecords([{ state: big, historyLines: 2 ** 40 + 1 }]))
    expect(r!.state).toBe(big)
    expect(r!.historyLines).toBe(2 ** 40 + 1)
  })

  it('returns an empty list for an empty message', () => {
    expect(parseRecords(new Uint8Array())).toEqual([])
  })

  it('throws RecordParseError with the good prefix on a truncated header', () => {
    const good = encodeRecords([{ payload: 'a' }])
    const msg = new Uint8Array(good.length + 10)
    msg.set(good)
    try {
      parseRecords(msg)
      expect.unreachable()
    } catch (e) {
      expect(e).toBeInstanceOf(RecordParseError)
      expect((e as RecordParseError).parsed).toHaveLength(1)
    }
  })

  it('throws on a truncated payload', () => {
    const msg = encodeRecords([{ payload: 'hello world' }]).subarray(0, RECORD_HEADER_LEN + 3)
    expect(() => parseRecords(msg)).toThrow(RecordParseError)
  })
})

describe('toBytes', () => {
  it('accepts ArrayBuffer, typed views and number arrays', () => {
    const ab = new Uint8Array([1, 2, 3]).buffer
    expect([...toBytes(ab)]).toEqual([1, 2, 3])
    expect([...toBytes(new Uint8Array([4, 5]))]).toEqual([4, 5])
    expect([...toBytes(new DataView(new Uint8Array([6]).buffer))]).toEqual([6])
    expect([...toBytes([7, 8])]).toEqual([7, 8])
    expect(() => toBytes('nope')).toThrow(TypeError)
  })
})
