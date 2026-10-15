// Binary channel framing for `attach_pane` → `onOutput` (docs/INTERFACES.md §6.2).
//
// Each channel message holds one or more records back to back. All integers
// are little-endian.
//
// offset size field
// 0      1    kind      1 = screen, 2 = input-ack, 3 = pane-closed, 4 = predict
// 1      1    flags     bit0 FULL, bit1 RESIZED, bit2 HISTORY_GAP
// 2      2    cols
// 4      2    rows
// 6      2    reserved (0)
// 8      8    state
// 16     8    ackedInputSeq
// 24     8    historyStart
// 32     8    historyLines
// 40     4    payloadLen
// 44     n    payload (VT bytes; empty for kinds 2 and 3)

export const RECORD_HEADER_LEN = 44

export const RecordKind = {
  Screen: 1,
  InputAck: 2,
  PaneClosed: 3,
  Predict: 4
} as const
export type RecordKindValue = (typeof RecordKind)[keyof typeof RecordKind]

export const RecordFlag = {
  Full: 1 << 0,
  Resized: 1 << 1,
  HistoryGap: 1 << 2
} as const

export type TermRecord = {
  kind: number
  flags: number
  cols: number
  rows: number
  state: number
  ackedInputSeq: number
  historyStart: number
  historyLines: number
  /** View into the channel message (no copy). Never transform it. */
  payload: Uint8Array
}

export class RecordParseError extends Error {
  constructor(
    message: string,
    /** Records that parsed cleanly before the error. */
    readonly parsed: TermRecord[]
  ) {
    super(message)
    this.name = 'RecordParseError'
  }
}

export const isFull = (r: TermRecord): boolean => (r.flags & RecordFlag.Full) !== 0
export const isResized = (r: TermRecord): boolean => (r.flags & RecordFlag.Resized) !== 0
export const hasHistoryGap = (r: TermRecord): boolean => (r.flags & RecordFlag.HistoryGap) !== 0

const TWO_POW_32 = 0x1_0000_0000

/** u64 LE → JS number. Values stay far below 2^53 in practice (contract §6.1). */
function readU64(view: DataView, offset: number): number {
  const lo = view.getUint32(offset, true)
  const hi = view.getUint32(offset + 4, true)
  return hi * TWO_POW_32 + lo
}

function writeU64(view: DataView, offset: number, value: number): void {
  view.setUint32(offset, value % TWO_POW_32, true)
  view.setUint32(offset + 4, Math.floor(value / TWO_POW_32), true)
}

/**
 * Normalize whatever the Tauri channel handed us into bytes without copying
 * when possible (Tauri 2 delivers `InvokeResponseBody::Raw` as ArrayBuffer).
 */
export function toBytes(message: unknown): Uint8Array {
  if (message instanceof ArrayBuffer) return new Uint8Array(message)
  if (ArrayBuffer.isView(message)) {
    return new Uint8Array(message.buffer, message.byteOffset, message.byteLength)
  }
  if (Array.isArray(message)) return Uint8Array.from(message as number[])
  throw new TypeError(`unexpected channel message type: ${Object.prototype.toString.call(message)}`)
}

/** Parse every record in one channel message. Throws RecordParseError on truncation. */
export function parseRecords(message: ArrayBuffer | Uint8Array): TermRecord[] {
  const bytes = message instanceof Uint8Array ? message : new Uint8Array(message)
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const out: TermRecord[] = []
  let off = 0
  while (off < bytes.byteLength) {
    if (bytes.byteLength - off < RECORD_HEADER_LEN) {
      throw new RecordParseError(
        `truncated record header at offset ${off} (${bytes.byteLength - off} bytes left)`,
        out
      )
    }
    const payloadLen = view.getUint32(off + 40, true)
    const payloadStart = off + RECORD_HEADER_LEN
    const end = payloadStart + payloadLen
    if (end > bytes.byteLength) {
      throw new RecordParseError(
        `truncated record payload at offset ${off}: need ${payloadLen}, have ${bytes.byteLength - payloadStart}`,
        out
      )
    }
    out.push({
      kind: view.getUint8(off),
      flags: view.getUint8(off + 1),
      cols: view.getUint16(off + 2, true),
      rows: view.getUint16(off + 4, true),
      state: readU64(view, off + 8),
      ackedInputSeq: readU64(view, off + 16),
      historyStart: readU64(view, off + 24),
      historyLines: readU64(view, off + 32),
      payload: bytes.subarray(payloadStart, end)
    })
    off = end
  }
  return out
}

export type RecordInit = Partial<Omit<TermRecord, 'payload'>> & {
  payload?: Uint8Array | string
}

const encoder = new TextEncoder()

/** Encode records into one channel message (used by the mock backend and tests). */
export function encodeRecords(records: RecordInit[]): Uint8Array {
  const payloads = records.map((r) =>
    typeof r.payload === 'string' ? encoder.encode(r.payload) : (r.payload ?? new Uint8Array())
  )
  const total = payloads.reduce((n, p) => n + RECORD_HEADER_LEN + p.byteLength, 0)
  const out = new Uint8Array(total)
  const view = new DataView(out.buffer)
  let off = 0
  records.forEach((r, i) => {
    const payload = payloads[i]!
    view.setUint8(off, r.kind ?? RecordKind.Screen)
    view.setUint8(off + 1, r.flags ?? 0)
    view.setUint16(off + 2, r.cols ?? 0, true)
    view.setUint16(off + 4, r.rows ?? 0, true)
    view.setUint16(off + 6, 0, true)
    writeU64(view, off + 8, r.state ?? 0)
    writeU64(view, off + 16, r.ackedInputSeq ?? 0)
    writeU64(view, off + 24, r.historyStart ?? 0)
    writeU64(view, off + 32, r.historyLines ?? 0)
    view.setUint32(off + 40, payload.byteLength, true)
    out.set(payload, off + RECORD_HEADER_LEN)
    off += RECORD_HEADER_LEN + payload.byteLength
  })
  return out
}
