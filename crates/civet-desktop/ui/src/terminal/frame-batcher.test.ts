import { describe, expect, it, vi } from 'vitest'
import { FrameBatcher, type FrameScheduler } from './frame-batcher'

function manualScheduler() {
  let next = 1
  const pending = new Map<number, () => void>()
  const scheduler: FrameScheduler = {
    request: (cb) => {
      const id = next++
      pending.set(id, cb)
      return id
    },
    cancel: (id) => void pending.delete(id)
  }
  const frame = () => {
    const cbs = [...pending.values()]
    pending.clear()
    cbs.forEach((cb) => cb())
  }
  return { scheduler, frame, pending }
}

describe('FrameBatcher', () => {
  it('coalesces everything pushed within a frame into one flush, in order', () => {
    const { scheduler, frame, pending } = manualScheduler()
    const flush = vi.fn()
    const b = new FrameBatcher<number>(flush, scheduler)
    b.push(1)
    b.pushAll([2, 3])
    b.push(4)
    expect(pending.size).toBe(1) // one rAF requested, not four
    expect(flush).not.toHaveBeenCalled()
    frame()
    expect(flush).toHaveBeenCalledTimes(1)
    expect(flush).toHaveBeenCalledWith([1, 2, 3, 4])
    expect(b.pending).toBe(0)
  })

  it('schedules a new frame for items arriving after a flush', () => {
    const { scheduler, frame } = manualScheduler()
    const flush = vi.fn()
    const b = new FrameBatcher<string>(flush, scheduler)
    b.push('a')
    frame()
    b.push('b')
    frame()
    expect(flush.mock.calls).toEqual([[['a']], [['b']]])
  })

  it('does not schedule for empty pushAll', () => {
    const { scheduler, pending } = manualScheduler()
    const b = new FrameBatcher<number>(vi.fn(), scheduler)
    b.pushAll([])
    expect(pending.size).toBe(0)
  })

  it('flushNow applies synchronously and cancels the pending frame', () => {
    const { scheduler, frame, pending } = manualScheduler()
    const flush = vi.fn()
    const b = new FrameBatcher<number>(flush, scheduler)
    b.push(1)
    b.flushNow()
    expect(flush).toHaveBeenCalledWith([1])
    expect(pending.size).toBe(0)
    frame()
    expect(flush).toHaveBeenCalledTimes(1)
  })

  it('drops queued items and ignores pushes after dispose', () => {
    const { scheduler, frame } = manualScheduler()
    const flush = vi.fn()
    const b = new FrameBatcher<number>(flush, scheduler)
    b.push(1)
    b.dispose()
    b.push(2)
    frame()
    expect(flush).not.toHaveBeenCalled()
  })

  it('falls back to a timer when requestAnimationFrame is unavailable', async () => {
    vi.useFakeTimers()
    try {
      const flush = vi.fn()
      const b = new FrameBatcher<number>(flush)
      b.push(1)
      b.push(2)
      vi.advanceTimersByTime(20)
      expect(flush).toHaveBeenCalledWith([1, 2])
    } finally {
      vi.useRealTimers()
    }
  })
})
