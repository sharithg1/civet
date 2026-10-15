import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { MockBackend } from './backend.mock'
import { PaneStream, type TerminalSink } from './terminal/pane-stream'
import { RecordKind, isFull, parseRecords, type TermRecord } from './terminal/records'
import type { HostStateEvent } from './types'

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

async function connected() {
  const b = new MockBackend()
  const states: HostStateEvent[] = []
  await b.onHostState((e) => states.push(e))
  const p = b.connectHost({ kind: 'ssh', destination: 'devbox' })
  await vi.runAllTimersAsync()
  return { b, summary: await p, states }
}

describe('MockBackend', () => {
  it('connects with progress and lists repos, worktrees and panes', async () => {
    const { b, summary, states } = await connected()
    expect(summary).toMatchObject({ hostId: 'devbox', state: 'connected', transport: 'quic' })
    expect(states.map((s) => s.state)).toEqual(['connecting', 'connected'])
    const repos = await b.listRepos('devbox')
    expect(repos.map((r) => r.id)).toEqual(['civet', 'orca'])
    expect((await b.listWorktrees('devbox', 'civet')).length).toBe(3)
    expect((await b.listPanes('devbox')).length).toBe(2)
  })

  it('streams a Full then echo diffs through valid binary records', async () => {
    const { b } = await connected()
    const pane = (await b.listPanes('devbox')).find((p) => !p.agent)!
    const records: TermRecord[] = []
    const att = await b.attachPane('devbox', pane.id, (m) => records.push(...parseRecords(m)))
    await vi.advanceTimersByTimeAsync(10)
    expect(records[0]!.kind).toBe(RecordKind.Screen)
    expect(isFull(records[0]!)).toBe(true)
    await b.writeInput('devbox', pane.id, 'hi')
    await vi.advanceTimersByTimeAsync(10)
    const last = records.at(-1)!
    expect(new TextDecoder().decode(last.payload)).toBe('hi')
    expect(last.state).toBeGreaterThan(records[0]!.state)

    // Records drive a PaneStream / terminal sink end to end, including resize.
    const sink: TerminalSink & { cols: number; rows: number } = {
      cols: 1,
      rows: 1,
      resize(c, r) {
        this.cols = c
        this.rows = r
      },
      write() {}
    }
    const stream = new PaneStream()
    stream.apply(records, sink)
    expect([sink.cols, sink.rows]).toEqual([pane.size.cols, pane.size.rows])
    await b.resizePane('devbox', pane.id, { cols: 90, rows: 20, pixelWidth: 0, pixelHeight: 0 })
    await vi.advanceTimersByTimeAsync(10)
    stream.apply(records.slice(-1), sink)
    expect([sink.cols, sink.rows]).toEqual([90, 20])
    await att.detach()
  })

  it('serves scrollback pages and emits pane-closed on kill', async () => {
    const { b } = await connected()
    const pane = (await b.listPanes('devbox'))[1]!
    const page = b.fetchScrollback('devbox', pane.id, { before: { end: 1000, limit: 10 } })
    await vi.advanceTimersByTimeAsync(200)
    expect((await page).start).toBe(990)
    const kinds: number[] = []
    await b.attachPane('devbox', pane.id, (m) => kinds.push(...parseRecords(m).map((r) => r.kind)))
    await b.killPane('devbox', pane.id)
    await vi.advanceTimersByTimeAsync(10)
    expect(kinds).toContain(RecordKind.PaneClosed)
  })

  it('rejects unknown hosts with a CivetError code', async () => {
    const b = new MockBackend()
    await expect(b.listRepos('nope')).rejects.toMatchObject({ code: 'noSuchHost' })
  })
})
