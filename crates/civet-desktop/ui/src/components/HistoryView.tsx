// Read-only view of older scrollback fetched page by page from the daemon,
// shown above the live terminal (xterm.js cannot prepend to its own
// scrollback; see docs/ARCHITECTURE.md "Scrollback"). New for Civet; styling
// follows Orca's terminal pane chrome.

import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { History, X } from 'lucide-react'
import type { PaneController } from '@/terminal/pane-controller'
import type { LineRange } from '@/terminal/pane-stream'
import { historyToVt, type HistoryLine } from '@/terminal/scrollback-pager'
import { buildDefaultTerminalOptions } from '@/terminal/terminal-options'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { IconButton } from './IconButton'

/** Small outlined header action (was a bordered <button>). */
const HEADER_ACTION =
  'h-5 rounded border-border bg-transparent px-1.5 text-xs font-normal text-muted-foreground shadow-none hover:bg-muted hover:text-foreground dark:border-border dark:bg-transparent dark:hover:bg-muted'

type Props = {
  controller: PaneController
  gaps: LineRange[]
  onClose: () => void
}

export function HistoryView({ controller, gaps, onClose }: Props) {
  const pager = controller.pager
  const hostRef = useRef<HTMLDivElement>(null)
  const termRef = useRef<Terminal | null>(null)
  const [gapView, setGapView] = useState<{ gap: LineRange; lines: HistoryLine[] } | null>(null)
  const [gapError, setGapError] = useState<string | null>(null)
  const version = useSyncExternalStore(
    (fn) => pager.subscribe(fn),
    () => `${pager.lines.length}:${pager.loading}:${pager.error}:${pager.oldestAvailable}`
  )

  // One read-only xterm (DOM renderer: no extra WebGL context) for the history.
  useEffect(() => {
    const el = hostRef.current
    if (!el) return
    const term = new Terminal({
      ...buildDefaultTerminalOptions(),
      disableStdin: true,
      cursorBlink: false,
      cursorStyle: 'bar',
      cursorInactiveStyle: 'none',
      scrollback: 100_000,
      cols: controller.term.cols,
      rows: 10
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(el)
    termRef.current = term
    const fitRows = () => {
      const dims = fit.proposeDimensions()
      if (dims && Number.isFinite(dims.rows) && dims.rows > 0) {
        term.resize(controller.term.cols, Math.max(1, Math.floor(dims.rows)))
      }
    }
    const ro = new ResizeObserver(fitRows)
    ro.observe(el)
    const sub = term.onScroll((y) => {
      if (y === 0 && !pager.loading) void controller.loadOlderHistory()
    })
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0 && term.buffer.active.viewportY === 0 && !pager.loading) {
        void controller.loadOlderHistory()
      }
    }
    el.addEventListener('wheel', onWheel, { passive: true })
    return () => {
      ro.disconnect()
      sub.dispose()
      el.removeEventListener('wheel', onWheel)
      term.dispose()
      termRef.current = null
    }
  }, [controller, pager])

  // Re-render content when pages arrive, keeping the previously visible line in place.
  const renderedCount = useRef(0)
  useEffect(() => {
    const term = termRef.current
    if (!term) return
    const lines = gapView ? gapView.lines : pager.lines
    const added = gapView ? 0 : Math.max(0, lines.length - renderedCount.current)
    const prevTop = term.buffer.active.viewportY
    renderedCount.current = gapView ? 0 : lines.length
    term.reset()
    if (lines.length === 0) return
    term.write(historyToVt(lines), () => {
      if (gapView) term.scrollToTop()
      else if (renderedCount.current === added) term.scrollToBottom()
      else term.scrollToLine(prevTop + added)
    })
  }, [version, gapView, pager])

  const loaded = pager.lines
  const first = loaded[0]?.line
  const last = loaded[loaded.length - 1]?.line
  const exhausted = pager.exhausted(controller.oldestLocalLine())

  async function showGap(gap: LineRange) {
    setGapError(null)
    try {
      setGapView({ gap, lines: await pager.loadGap(gap) })
    } catch (err) {
      setGapError((err as Error).message ?? String(err))
    }
  }

  return (
    <div className="flex h-[45%] min-h-24 flex-col border-b border-border bg-chrome">
      <div className="flex h-7 shrink-0 items-center gap-2 px-3 font-mono text-2xs text-faint">
        <History className="size-3.5" aria-hidden />
        {gapView ? (
          <span>
            Skipped lines {gapView.gap.start}–{gapView.gap.end - 1}
            <Button
              type="button"
              variant="link"
              className="ml-2 h-auto p-0 text-xs font-normal text-muted-foreground underline hover:text-foreground"
              onClick={() => setGapView(null)}
            >
              back to history
            </Button>
          </span>
        ) : (
          <span>
            {first === undefined ? 'Scrollback history' : `History lines ${first}–${last}`}
            {exhausted && first !== undefined ? ' · start of retained history' : ''}
          </span>
        )}
        {pager.loading && <Spinner className="size-3.5" />}
        {pager.error && <span className="text-destructive">{pager.error}</span>}
        {gapError && <span className="text-destructive">{gapError}</span>}
        <div className="ml-auto flex items-center gap-2">
          {!gapView &&
            gaps.slice(-3).map((g) => (
              <Tooltip key={`${g.start}-${g.end}`}>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    variant="outline"
                    size="xs"
                    className={HEADER_ACTION}
                    onClick={() => void showGap(g)}
                  >
                    {g.end - g.start} skipped lines
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Output skipped while the view was catching up</TooltipContent>
              </Tooltip>
            ))}
          {!gapView && !exhausted && (
            <Button
              type="button"
              variant="outline"
              size="xs"
              className={HEADER_ACTION}
              onClick={() => void controller.loadOlderHistory()}
              disabled={pager.loading}
            >
              Load older
            </Button>
          )}
          <IconButton
            label="Close history"
            className="size-5 rounded hover:bg-muted dark:hover:bg-muted"
            onClick={onClose}
          >
            <X className="size-3.5" />
          </IconButton>
        </div>
      </div>
      <div ref={hostRef} className="terminal-host history-host relative min-h-0 flex-1" />
    </div>
  )
}
