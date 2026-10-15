// Imperative owner of one xterm.js instance bound to one daemon pane:
// attach_pane channel → parse records → per-frame batch → PaneStream.apply,
// input → write_input, container size → debounced resize_pane, scroll-to-top
// → lazy scrollback. Ideas ported from Orca's src/renderer/src/lib/pane-manager/
// (pane-lifecycle.ts, pane-fit-resize-observer.ts, pane-terminal-output-scheduler.ts),
// minus Orca's local-PTY plumbing: here the server owns the PTY size.

import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { Unicode11Addon } from '@xterm/addon-unicode11'
import type { CivetBackend, PaneAttachment } from '../backend'
import { termSize, type PaneId } from '../types'
import { FrameBatcher } from './frame-batcher'
import { PaneStream, type LineRange } from './pane-stream'
import { RecordParseError, parseRecords, type TermRecord } from './records'
import { ScrollbackPager } from './scrollback-pager'
import { handleClipboardKey, isMacPlatform } from './clipboard-keys'
import { buildDefaultTerminalOptions } from './terminal-options'
import { attachWebgl, type WebglHandle } from './webgl'

export const RESIZE_DEBOUNCE_MS = 100

export type PaneControllerCallbacks = {
  onClosed: () => void
  onRenderer: (renderer: 'webgl' | 'dom', reason?: string) => void
  onScrollTop: () => void
  onGap: (gap: LineRange) => void
  onError: (message: string) => void
}

export class PaneController {
  readonly term: Terminal
  readonly stream: PaneStream
  readonly pager: ScrollbackPager
  private readonly fit = new FitAddon()
  private readonly batcher: FrameBatcher<TermRecord>
  private webgl: WebglHandle | null = null
  private attachment: PaneAttachment | null = null
  private disposed = false
  private resizeObserver: ResizeObserver | null = null
  private resizeTimer: ReturnType<typeof setTimeout> | null = null
  private lastRequested: { cols: number; rows: number } | null = null
  private container: HTMLElement | null = null
  private sawRecord = false
  private readonly cleanups: (() => void)[] = []

  constructor(
    private readonly backend: CivetBackend,
    readonly hostId: string,
    readonly pane: PaneId,
    private readonly cb: PaneControllerCallbacks
  ) {
    this.term = new Terminal(buildDefaultTerminalOptions())
    this.stream = new PaneStream({
      onClosed: () => {
        this.term.options.disableStdin = true
        this.cb.onClosed()
      },
      onGap: (gap) => this.cb.onGap(gap)
    })
    this.batcher = new FrameBatcher<TermRecord>((records) => {
      this.sawRecord = true
      this.stream.apply(records, this.term)
    })
    this.pager = new ScrollbackPager((query) =>
      this.backend.fetchScrollback(this.hostId, this.pane, query)
    )
  }

  mount(container: HTMLElement): void {
    this.container = container
    this.term.loadAddon(this.fit)
    // Match the daemon's (alacritty_terminal) character widths: emoji and
    // other wide chars take two cells, so diffs from the server line up.
    this.term.loadAddon(new Unicode11Addon())
    this.term.unicode.activeVersion = '11'
    this.term.open(container)
    this.webgl = attachWebgl(this.term, (reason) => this.cb.onRenderer('dom', reason))
    if (this.webgl.active) this.cb.onRenderer('webgl')

    const d1 = this.term.onData((data) => {
      if (this.stream.closed) return
      this.backend
        .writeInput(this.hostId, this.pane, data)
        .catch((err: unknown) => this.cb.onError(`input: ${String((err as Error).message ?? err)}`))
    })
    const d2 = this.term.onBinary((data) => {
      if (this.stream.closed) return
      this.backend
        .writeInputBinary(this.hostId, this.pane, data)
        .catch((err: unknown) => this.cb.onError(`input: ${String((err as Error).message ?? err)}`))
    })
    const d3 = this.term.onScroll((y) => {
      const buf = this.term.buffer.active
      if (y === 0 && buf.type === 'normal' && buf.baseY > 0) this.cb.onScrollTop()
    })
    this.cleanups.push(() => d1.dispose(), () => d2.dispose(), () => d3.dispose())

    // Cmd+C copies xterm's selection, Cmd+A selects all (see clipboard-keys).
    const isMac = isMacPlatform()
    this.term.attachCustomKeyEventHandler((ev) =>
      handleClipboardKey(ev, this.term, {
        isMac,
        copy: (text) => {
          this.backend.writeClipboard(text).catch((err: unknown) =>
            this.cb.onError(`copy: ${String((err as Error).message ?? err)}`)
          )
        }
      })
    )

    // Scrolling up while already at the top (or with no local scrollback yet).
    const onWheel = (e: WheelEvent) => {
      const buf = this.term.buffer.active
      if (e.deltaY < 0 && buf.type === 'normal' && buf.viewportY === 0) this.cb.onScrollTop()
    }
    container.addEventListener('wheel', onWheel, { passive: true })
    this.cleanups.push(() => container.removeEventListener('wheel', onWheel))

    this.resizeObserver = new ResizeObserver(() => this.scheduleResize())
    this.resizeObserver.observe(container)

    void this.attach()
  }

  private async attach(): Promise<void> {
    try {
      const attachment = await this.backend.attachPane(this.hostId, this.pane, (message) =>
        this.onMessage(message)
      )
      if (this.disposed) {
        void attachment.detach()
        return
      }
      this.attachment = attachment
      const { cols, rows } = attachment.info.size
      // The first record is a Full carrying the size; only pre-size if it has
      // not been applied yet so the initial paint lands at the right geometry.
      if (!this.sawRecord && cols > 0 && rows > 0 && (cols !== this.term.cols || rows !== this.term.rows)) {
        this.batcher.flushNow()
        if (!this.sawRecord) this.term.resize(cols, rows)
      }
      this.scheduleResize(0)
    } catch (err) {
      if (!this.disposed) this.cb.onError(`attach failed: ${(err as Error).message ?? String(err)}`)
    }
  }

  private onMessage(message: Uint8Array): void {
    if (this.disposed) return
    try {
      this.batcher.pushAll(parseRecords(message))
    } catch (err) {
      if (err instanceof RecordParseError) {
        this.batcher.pushAll(err.parsed)
        console.error('[civet] malformed channel message', err.message)
      } else {
        throw err
      }
    }
  }

  /** Debounced resize_pane from the fit addon's proposal; xterm resizes when the server confirms. */
  scheduleResize(delay = RESIZE_DEBOUNCE_MS): void {
    if (this.disposed) return
    if (this.resizeTimer) clearTimeout(this.resizeTimer)
    this.resizeTimer = setTimeout(() => {
      this.resizeTimer = null
      this.requestResize()
    }, delay)
  }

  private requestResize(): void {
    if (this.disposed || !this.attachment || this.stream.closed) return
    if (!this.container || this.container.clientWidth === 0 || this.container.clientHeight === 0) {
      return // hidden tab: keep the current size
    }
    const dims = this.fit.proposeDimensions()
    if (!dims || !Number.isFinite(dims.cols) || !Number.isFinite(dims.rows)) return
    const cols = Math.max(2, Math.floor(dims.cols))
    const rows = Math.max(1, Math.floor(dims.rows))
    if (cols === this.term.cols && rows === this.term.rows) {
      this.lastRequested = { cols, rows }
      return
    }
    if (this.lastRequested?.cols === cols && this.lastRequested.rows === rows) return
    this.lastRequested = { cols, rows }
    this.backend
      .resizePane(this.hostId, this.pane, termSize(cols, rows))
      .catch((err: unknown) => {
        this.lastRequested = null
        this.cb.onError(`resize: ${(err as Error).message ?? String(err)}`)
      })
  }

  /** Oldest absolute history line still present in xterm's own scrollback. */
  oldestLocalLine(): number {
    return this.stream.oldestLocalLine(this.term.buffer.normal.baseY)
  }

  loadOlderHistory(): Promise<number> {
    return this.pager.loadOlder(this.oldestLocalLine())
  }

  focus(): void {
    this.term.focus()
  }

  /** Re-check the size, e.g. when the tab becomes visible again. */
  refit(): void {
    this.lastRequested = null
    this.scheduleResize(0)
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    if (this.resizeTimer) clearTimeout(this.resizeTimer)
    this.resizeObserver?.disconnect()
    this.cleanups.forEach((fn) => fn())
    this.batcher.dispose()
    void this.attachment?.detach().catch(() => {})
    this.webgl?.dispose()
    this.term.dispose()
  }
}
