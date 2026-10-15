// Per-animation-frame batching of terminal records.
//
// The Rust side already batches records per pane (4 ms / 256 KiB, INTERFACES
// §6.2). Channel messages can still arrive several times per frame under heavy
// output, so the pane queues parsed records and applies them to xterm.js once
// per animation frame: one resize check + a run of `term.write` calls, then
// xterm renders once. Keystroke echoes still land on the very next frame.

export type FrameScheduler = {
  request(cb: () => void): number
  cancel(handle: number): void
}

export const animationFrameScheduler: FrameScheduler = {
  request: (cb) =>
    typeof requestAnimationFrame === 'function'
      ? requestAnimationFrame(() => cb())
      : (setTimeout(cb, 16) as unknown as number),
  cancel: (handle) =>
    typeof cancelAnimationFrame === 'function'
      ? cancelAnimationFrame(handle)
      : clearTimeout(handle as unknown as ReturnType<typeof setTimeout>)
}

export class FrameBatcher<T> {
  private queue: T[] = []
  private handle: number | null = null
  private disposed = false

  constructor(
    private readonly flush: (items: T[]) => void,
    private readonly scheduler: FrameScheduler = animationFrameScheduler
  ) {}

  /** Number of items waiting for the next frame. */
  get pending(): number {
    return this.queue.length
  }

  push(item: T): void {
    if (this.disposed) return
    this.queue.push(item)
    this.schedule()
  }

  pushAll(items: readonly T[]): void {
    if (this.disposed || items.length === 0) return
    for (const item of items) this.queue.push(item)
    this.schedule()
  }

  /** Apply everything queued right now (e.g. before a synchronous resize). */
  flushNow(): void {
    if (this.handle !== null) {
      this.scheduler.cancel(this.handle)
      this.handle = null
    }
    this.run()
  }

  dispose(): void {
    this.disposed = true
    if (this.handle !== null) this.scheduler.cancel(this.handle)
    this.handle = null
    this.queue = []
  }

  private schedule(): void {
    if (this.handle !== null) return
    this.handle = this.scheduler.request(() => {
      this.handle = null
      this.run()
    })
  }

  private run(): void {
    if (this.disposed || this.queue.length === 0) return
    const items = this.queue
    this.queue = []
    this.flush(items)
  }
}
