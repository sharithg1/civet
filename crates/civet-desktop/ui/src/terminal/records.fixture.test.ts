// Cross-checks the binary record layout against the fixture that the Rust
// encoder (crates/civet-desktop/src/records.rs) is also tested against.
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { encodeRecords, parseRecords } from './records'

type FixtureRecord = {
  kind: number
  flags: number
  cols: number
  rows: number
  state: number
  ackedInputSeq: number
  historyStart: number
  historyLines: number
  payload: string
}

const fixture = JSON.parse(
  readFileSync(new URL('./records.fixture.json', import.meta.url), 'utf8')
) as { records: FixtureRecord[]; hex: string }

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2)
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  return out
}

const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')

describe('records fixture shared with Rust', () => {
  it('parses the Rust-encoded bytes into the fixture records', () => {
    const recs = parseRecords(fromHex(fixture.hex))
    expect(recs).toHaveLength(fixture.records.length)
    recs.forEach((r, i) => {
      const want = fixture.records[i]!
      const { payload, ...header } = r
      const { payload: wantPayload, ...wantHeader } = want
      expect(header).toEqual(wantHeader)
      expect(new TextDecoder().decode(payload)).toBe(wantPayload)
    })
  })

  it('encodes the fixture records into the same bytes', () => {
    expect(toHex(encodeRecords(fixture.records))).toBe(fixture.hex)
  })
})
