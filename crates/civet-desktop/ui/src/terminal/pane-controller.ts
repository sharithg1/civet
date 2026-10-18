// Imperative owner of one xterm.js instance bound to one daemon pane:
// attach_pane channel → parse records → per-frame batch → PaneStream.apply,
// input → write_input, container size → debounced resize_pane, scroll-to-top
// → lazy scrollback. Ideas ported from Orca's src/renderer/src/lib/pane-manager/
// (pane-lifecycle.ts, pane-fit-resize-observer.ts, pane-terminal-output-scheduler.ts),
// minus Orca's local-PTY plumbing: here the server owns the PTY size.

import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { Unicode11Addon } from '@xterm/addon-unicode11'
import { SearchAddon, type ISearchResultChangeEvent } from '@xterm/addon-search'
import type { CivetBackend, PaneAttachment } from '../backend'
import { termSize, type PaneId } from '../types'
import { FrameBatcher } from './frame-batcher'
import { PaneStream, type LineRange } from './pane-stream'
import { RecordParseError, parseRecords, type TermRecord } from './records'
import { ScrollbackPager } from './scrollback-pager'
import { handleClipboardKey, isMacPlatform } from './clipboard-keys'
import { handleShiftEnter } from './newline-keys'
import { handleFindKey, type FindAction, type FindOptions } from './find-keys'
import { InputQueue } from './input-queue'
import { FIND_DECORATIONS, buildDefaultTerminalOptions } from './terminal-options'
import {
  clipboardImage,
  clipboardImageName,
  dropChunks,
  isLargePaste,
  pasteFileName
} from './file-drop'
import { attachWebgl, type WebglHandle } from './webgl'

export const RESIZE_DEBOUNCE_MS = 100

/** Matches highlighted by the find bar; past this the counter shows "1000+". */
export const FIND_HIGHLIGHT_LIMIT = 1000

export type PaneControllerCallbacks = {
  onClosed: () => void
  onRenderer: (renderer: 'webgl' | 'dom', reason?: string) => void
  onScrollTop: () => void
  onGap: (gap: LineRange) => void
  onError: (message: string) => void
  /** Cmd+F / Cmd+G / Shift+Cmd+G in the terminal (see find-keys). */
  onFind?: (action: FindAction) => void
  /** A plain text paste (Cmd+V) was large: a hint toward Paste as File. */
  onLargePaste?: () => void
  /** False while the host is not connected: input errors are expected then (no toast). */
  canReportInputError?: () => boolean
}

export class PaneController {
  readonly term: Terminal
  readonly stream: PaneStream
  readonly pager: ScrollbackPager
  private readonly fit = new FitAddon()
  private readonly search = new SearchAddon({ highlightLimit: FIND_HIGHLIGHT_LIMIT })
  private readonly batcher: FrameBatcher<TermRecord>
  private webgl: WebglHandle | null = null
  private attachment: PaneAttachment | null = null
  private disposed = false
  private resizeObserver: ResizeObserver | null = null
  private resizeTimer: ReturnType<typeof setTimeout> | null = null
  private lastRequested: { cols: number; rows: number } | null = null
  private container: HTMLElement | null = null
  private sawRecord = false
  /** Set while a find scrolls to its match: that is not the user scrolling to the top. */
  private findScrolling = false
  private readonly cleanups: (() => void)[] = []
  /** Keeps this pane's keystrokes in order (one input invoke in flight). */
  private readonly input: InputQueue

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
    this.input = new InputQueue({
      send: (c) =>
        c.kind === 'text'
          ? this.backend.writeInput(this.hostId, this.pane, c.data)
          : this.backend.writeInputBinary(this.hostId, this.pane, c.data),
      onError: (err) => this.cb.onError(`input: ${String((err as Error).message ?? err)}`),
      shouldReport: () => !this.disposed && (this.cb.canReportInputError?.() ?? true)
    })
  }

  mount(container: HTMLElement): void {
    this.container = container
    this.term.loadAddon(this.fit)
    // Match the daemon's (alacritty_terminal) character widths: emoji and
    // other wide chars take two cells, so diffs from the server line up.
    this.term.loadAddon(new Unicode11Addon())
    this.term.unicode.activeVersion = '11'
    this.term.loadAddon(this.search)
    this.term.open(container)
    this.webgl = attachWebgl(this.term, (reason) => this.cb.onRenderer('dom', reason))
    if (this.webgl.active) this.cb.onRenderer('webgl')

    const d1 = this.term.onData((data) => {
      if (this.stream.closed) return
      this.input.push('text', data)
    })
    const d2 = this.term.onBinary((data) => {
      if (this.stream.closed) return
      this.input.push('binary', data)
    })
    const d3 = this.term.onScroll((y) => {
      const buf = this.term.buffer.active
      if (y === 0 && buf.type === 'normal' && buf.baseY > 0 && !this.findScrolling) {
        this.cb.onScrollTop()
      }
    })
    this.cleanups.push(
      () => d1.dispose(),
      () => d2.dispose(),
      () => d3.dispose()
    )

    // Shift+Enter inserts a newline (see newline-keys); Cmd+F finds (see
    // find-keys); Cmd+C copies xterm's selection, Cmd+A selects all (see
    // clipboard-keys).
    const isMac = isMacPlatform()
    const sendNewline = (data: string) => {
      if (!this.stream.closed) this.input.push('text', data)
    }
    this.term.attachCustomKeyEventHandler(
      (ev) =>
        handleShiftEnter(ev, sendNewline) &&
        handleFindKey(ev, { isMac, onFind: (action) => this.cb.onFind?.(action) }) &&
        handleClipboardKey(ev, this.term, {
          isMac,
          copy: (text) => {
            this.backend
              .writeClipboard(text)
              .catch((err: unknown) =>
                this.cb.onError(`copy: ${String((err as Error).message ?? err)}`)
              )
          }
        })
    )

    // Cmd+V with only an image on the clipboard (a screenshot): store it on
    // the pane's host and paste its path, which agents attach as an image.
    // Capture phase, ahead of xterm's own paste handler on its textarea.
    // A large text paste still goes to xterm unchanged; it only raises the
    // Paste as File tip.
    const onPaste = (e: ClipboardEvent) => {
      const image = clipboardImage(e.clipboardData)
      if (!image) {
        const text = e.clipboardData?.getData('text/plain') ?? ''
        if (!this.stream.closed && isLargePaste(text)) this.cb.onLargePaste?.()
        return
      }
      if (this.stream.closed) return
      e.preventDefault()
      e.stopImmediatePropagation()
      void this.pasteImage(image)
    }
    container.addEventListener('paste', onPaste, { capture: true })
    this.cleanups.push(() => container.removeEventListener('paste', onPaste, { capture: true }))

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
      if (
        !this.sawRecord &&
        cols > 0 &&
        rows > 0 &&
        (cols !== this.term.cols || rows !== this.term.rows)
      ) {
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
    this.backend.resizePane(this.hostId, this.pane, termSize(cols, rows)).catch((err: unknown) => {
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

  /**
   * Select the next / previous match of `query` in the loaded buffer (older
   * history still on the host is not searched) and highlight all of them.
   * `incremental` keeps the current match while it still matches (typing).
   */
  find(
    query: string,
    opts: FindOptions,
    direction: 'next' | 'previous',
    incremental = false
  ): boolean {
    if (!query) {
      this.clearFind()
      return false
    }
    const searchOpts = { ...opts, decorations: FIND_DECORATIONS }
    this.findScrolling = true
    try {
      return direction === 'next'
        ? this.search.findNext(query, { ...searchOpts, incremental })
        : this.search.findPrevious(query, searchOpts)
    } finally {
      this.findScrolling = false
      this.repaintAfterFind()
    }
  }

  /** Drop the find highlights; the selected match stays selected (Cmd+C). */
  clearFind(): void {
    this.search.clearDecorations()
    this.repaintAfterFind()
  }

  // The DOM renderer keeps painting a disposed highlight's background until
  // its row is redrawn (each search replaces all highlights).
  private repaintAfterFind(): void {
    this.term.refresh(0, this.term.rows - 1)
  }

  /** Match index / count after each search (and as new output arrives). */
  onFindResults(listener: (results: ISearchResultChangeEvent) => void): () => void {
    const d = this.search.onDidChangeResults(listener)
    return () => d.dispose()
  }

  /**
   * Hand dropped file paths (already on this pane's host) to the program:
   * images as a bracketed paste (agents attach them), others typed.
   */
  dropPaths(paths: readonly string[]): void {
    if (this.stream.closed) return
    for (const c of dropChunks(paths)) {
      // term.paste applies bracketed-paste framing when the program enabled it.
      if (c.kind === 'paste') this.term.paste(c.text)
      else this.input.push('text', c.text)
    }
    this.focus()
  }

  private async pasteImage(image: File): Promise<void> {
    try {
      const data = new Uint8Array(await image.arrayBuffer())
      const path = await this.backend.uploadFile(this.hostId, clipboardImageName(image.type), data)
      if (!this.disposed) this.dropPaths([path])
    } catch (err) {
      this.cb.onError(`paste image: ${String((err as Error).message ?? err)}`)
    }
  }

  /**
   * Paste as File (⇧⌘V): store the clipboard text as a file on the pane's
   * host and type its path, so an agent reads the file instead of taking
   * the whole text into its context.
   */
  async pasteAsFile(): Promise<void> {
    if (this.stream.closed) return
    try {
      const text = await this.backend.readClipboard()
      if (!text) {
        this.cb.onError('paste as file: the clipboard has no text')
        return
      }
      const data = new TextEncoder().encode(text)
      const path = await this.backend.uploadFile(this.hostId, pasteFileName(), data)
      if (!this.disposed) this.dropPaths([path])
    } catch (err) {
      this.cb.onError(`paste as file: ${String((err as Error).message ?? err)}`)
    }
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
    this.input.close()
    this.batcher.dispose()
    void this.attachment?.detach().catch(() => {})
    this.webgl?.dispose()
    this.term.dispose()
  }
}
