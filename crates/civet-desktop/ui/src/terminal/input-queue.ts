// Per-pane keystroke queue. Tauri runs every async command in its own task,
// so two `write_input` invokes fired back to back can reach the Rust core in
// either order (and get their input seqs in that order). This queue keeps at
// most one input invoke per pane in flight: the next one is sent when the
// previous resolved, and whatever was typed meanwhile is coalesced into one
// write (same kind only: `write_input` text vs `write_input_binary` bytes).
// Order is therefore exactly xterm's onData/onBinary order.
//
// Errors (host not connected while reconnecting, …) are reported at most once
// per `errorIntervalMs`, and only while `shouldReport()` says so, so a user
// typing into a pane whose host is reconnecting does not get a toast per key.

export type InputKind = 'text' | 'binary'
export type InputChunk = { kind: InputKind; data: string }

export type InputQueueOptions = {
  send: (chunk: InputChunk) => Promise<void>
  onError: (err: unknown) => void
  /** False while the host is not connected: errors are expected, stay quiet. */
  shouldReport?: () => boolean
  errorIntervalMs?: number
  now?: () => number
}

export const INPUT_ERROR_INTERVAL_MS = 5000

export class InputQueue {
  private pending: InputChunk[] = []
  private inFlight = false
  private lastError = -Infinity
  private closed = false
  private idleWaiters: (() => void)[] = []

  constructor(private readonly opts: InputQueueOptions) {}

  push(kind: InputKind, data: string): void {
    if (this.closed || data.length === 0) return
    const last = this.pending[this.pending.length - 1]
    if (last && last.kind === kind) last.data += data
    else this.pending.push({ kind, data })
    if (!this.inFlight) void this.pump()
  }

  /** Chunks waiting behind the in-flight write (tests, diagnostics). */
  get queued(): number {
    return this.pending.length
  }

  /** Resolves once everything pushed so far has been sent (or failed). */
  idle(): Promise<void> {
    if (!this.inFlight && this.pending.length === 0) return Promise.resolve()
    return new Promise((r) => this.idleWaiters.push(r))
  }

  /** Drop queued input and stop accepting more (pane disposed). */
  close(): void {
    this.closed = true
    this.pending = []
  }

  private async pump(): Promise<void> {
    this.inFlight = true
    try {
      while (this.pending.length > 0) {
        const chunk = this.pending.shift()!
        try {
          await this.opts.send(chunk)
        } catch (err) {
          this.report(err)
        }
      }
    } finally {
      this.inFlight = false
      const waiters = this.idleWaiters
      this.idleWaiters = []
      waiters.forEach((w) => w())
    }
  }

  private report(err: unknown): void {
    if (this.closed) return
    if (this.opts.shouldReport && !this.opts.shouldReport()) return
    const now = (this.opts.now ?? Date.now)()
    if (now - this.lastError < (this.opts.errorIntervalMs ?? INPUT_ERROR_INTERVAL_MS)) return
    this.lastError = now
    this.opts.onError(err)
  }
}
