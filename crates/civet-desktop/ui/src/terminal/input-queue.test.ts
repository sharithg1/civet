import { describe, expect, it, vi } from 'vitest'
import { InputQueue, type InputChunk } from './input-queue'

/** A backend whose invokes resolve in an order the test chooses (like Tauri's concurrent tasks). */
function controllable() {
  const calls: { chunk: InputChunk; resolve: () => void; reject: (e: unknown) => void }[] = []
  const applied: string[] = []
  const send = vi.fn(
    (chunk: InputChunk) =>
      new Promise<void>((resolve, reject) => {
        calls.push({
          chunk,
          resolve: () => {
            applied.push(chunk.data)
            resolve()
          },
          reject
        })
      })
  )
  return { calls, applied, send }
}

const flush = () => new Promise((r) => setTimeout(r, 0))

describe('InputQueue', () => {
  it('keeps one write in flight per pane and preserves keystroke order', async () => {
    const { calls, applied, send } = controllable()
    const q = new InputQueue({ send, onError: () => {} })
    q.push('text', 'l')
    q.push('text', 's')
    q.push('text', ' -')
    expect(send).toHaveBeenCalledTimes(1)
    expect(calls[0]!.chunk).toEqual({ kind: 'text', data: 'l' })
    // Typed while "l" was in flight: coalesced into one ordered write.
    calls[0]!.resolve()
    await flush()
    expect(send).toHaveBeenCalledTimes(2)
    expect(calls[1]!.chunk).toEqual({ kind: 'text', data: 's -' })
    calls[1]!.resolve()
    await q.idle()
    expect(applied.join('')).toBe('ls -')
  })

  it('never reorders text and binary input', async () => {
    const { calls, send } = controllable()
    const q = new InputQueue({ send, onError: () => {} })
    q.push('text', 'a')
    q.push('binary', '\x1b[M !!')
    q.push('text', 'b')
    q.push('text', 'c')
    q.push('binary', '\x00')
    for (let i = 0; i < 4; i++) {
      calls[i]!.resolve()
      await flush()
    }
    await q.idle()
    expect(calls.map((c) => c.chunk)).toEqual([
      { kind: 'text', data: 'a' },
      { kind: 'binary', data: '\x1b[M !!' },
      { kind: 'text', data: 'bc' },
      { kind: 'binary', data: '\x00' }
    ])
  })

  it('a burst of 500 keystrokes arrives in order even when the transport is slow', async () => {
    const received: string[] = []
    const q = new InputQueue({
      // Random per-call latency: concurrent invokes would finish out of order.
      send: async (c) => {
        await new Promise((r) => setTimeout(r, Math.random() * 3))
        received.push(c.data)
      },
      onError: () => {}
    })
    let typed = ''
    for (let i = 0; i < 500; i++) {
      const ch = String.fromCharCode(33 + (i % 90))
      typed += ch
      q.push('text', ch)
      if (i % 7 === 0) await new Promise((r) => setTimeout(r, 1))
    }
    await q.idle()
    expect(received.join('')).toBe(typed)
  })

  it('keeps going after a failed write and throttles error reports', async () => {
    let t = 0
    const onError = vi.fn()
    let fail = true
    const sent: string[] = []
    const q = new InputQueue({
      send: async (c) => {
        if (fail) throw new Error('host is not connected')
        sent.push(c.data)
      },
      onError,
      now: () => t
    })
    for (const ch of 'abcdef') {
      q.push('text', ch)
      await q.idle()
      t += 100
    }
    expect(onError).toHaveBeenCalledTimes(1)
    t += 5000
    q.push('text', 'g')
    await q.idle()
    expect(onError).toHaveBeenCalledTimes(2)
    fail = false
    q.push('text', 'h')
    await q.idle()
    expect(sent).toEqual(['h'])
  })

  it('stays quiet while the host is not connected', async () => {
    const onError = vi.fn()
    let connected = false
    const q = new InputQueue({
      send: async () => {
        throw new Error('host is not connected')
      },
      onError,
      shouldReport: () => connected
    })
    for (const ch of 'typing during a reconnect') q.push('text', ch)
    await q.idle()
    expect(onError).not.toHaveBeenCalled()
    connected = true
    q.push('text', 'x')
    await q.idle()
    expect(onError).toHaveBeenCalledTimes(1)
  })

  it('drops input after close', async () => {
    const { send } = controllable()
    const q = new InputQueue({ send, onError: () => {} })
    q.close()
    q.push('text', 'x')
    expect(send).not.toHaveBeenCalled()
  })
})
