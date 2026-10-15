// One terminal leaf in a split layout. Pane chrome (focus dimming, exited
// state) follows Orca's terminal-pane; the PTY/IPC plumbing is PaneController
// (attach_pane binary channel). v3: a lone pane has no title strip (its
// actions float top-right on hover); split panes get a slim 22px strip.

import { useEffect, useRef, useState } from 'react'
import { Columns2, History, Rows2, X } from 'lucide-react'
import { useStore, closePanes, focusPane, splitFocused, reportError } from '@/state/store'
import { PaneController } from '@/terminal/pane-controller'
import { registerPane } from '@/terminal/pane-registry'
import type { LineRange } from '@/terminal/pane-stream'
import type { PaneId } from '@/types'
import { cn } from '@/lib/utils'
import { paneDot } from '@/lib/worktree-view'
import { HistoryView } from './HistoryView'
import { IconButton } from './IconButton'
import { StatusDot } from './StatusDot'

type Props = {
  hostId: string
  pane: PaneId
  focused: boolean
  multiPane: boolean
}

export function TerminalPane({ hostId, pane, focused, multiPane }: Props) {
  const backend = useStore((s) => s.backend)
  const info = useStore((s) => s.panes[hostId]?.[pane])
  const hostState = useStore((s) => s.hosts[hostId]?.state)
  const containerRef = useRef<HTMLDivElement>(null)
  const [controller, setController] = useState<PaneController | null>(null)
  const [closed, setClosed] = useState(false)
  const [renderer, setRenderer] = useState<{ kind: 'webgl' | 'dom'; reason?: string } | null>(null)
  const [historyOpen, setHistoryOpen] = useState(false)
  const [gaps, setGaps] = useState<LineRange[]>([])

  useEffect(() => {
    const el = containerRef.current
    if (!el || !backend) return
    const c = new PaneController(backend, hostId, pane, {
      onClosed: () => setClosed(true),
      onRenderer: (kind, reason) => {
        setRenderer({ kind, reason })
        if (kind === 'dom' && reason) console.info(`[civet] pane ${pane}: DOM renderer (${reason})`)
      },
      onScrollTop: () => {
        setHistoryOpen(true)
        void c.loadOlderHistory()
      },
      onGap: (gap) => setGaps((g) => [...g.slice(-9), gap]),
      onError: (message) => reportError(`Pane ${pane}`, message),
      canReportInputError: () => useStore.getState().hosts[hostId]?.state === 'connected'
    })
    c.mount(el)
    const unregister = registerPane(c)
    setController(c)
    return () => {
      unregister()
      c.dispose()
      setController(null)
    }
  }, [backend, hostId, pane])

  useEffect(() => {
    if (focused) controller?.focus()
  }, [focused, controller])

  // A host coming back may have resized while away; re-check our size.
  useEffect(() => {
    if (hostState === 'connected') controller?.refit()
  }, [hostState, controller])

  const exited = closed || info?.exited === true

  const title = info?.title || info?.command.join(' ') || `pane ${pane}`
  const actions = (
    <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover/pane:opacity-100 focus-within:opacity-100">
      <PaneButton label="Show older history" onClick={() => setHistoryOpen((v) => !v)}>
        <History className="size-3" />
      </PaneButton>
      <PaneButton label="Split right" onClick={() => void splitFocusedFrom(hostId, pane, 'row')}>
        <Columns2 className="size-3" />
      </PaneButton>
      <PaneButton label="Split down" onClick={() => void splitFocusedFrom(hostId, pane, 'column')}>
        <Rows2 className="size-3" />
      </PaneButton>
      <PaneButton label="Close pane" onClick={() => closePanes(hostId, [pane])}>
        <X className="size-3" />
      </PaneButton>
    </div>
  )

  return (
    <div
      className={cn(
        'group/pane relative flex h-full w-full min-w-0 flex-col overflow-hidden bg-background',
        multiPane && 'pane-multi',
        multiPane && (focused ? 'pane-focused' : 'pane-unfocused')
      )}
      onMouseDown={() => focusPane(hostId, pane)}
      data-pane-id={pane}
      data-host-id={hostId}
    >
      {multiPane ? (
        <div className="flex h-[22px] shrink-0 items-center gap-1.5 px-3 font-mono text-2xs text-faint">
          {info?.agent && <StatusDot kind={paneDot(info, hostState === 'connected')} />}
          <span className={cn('min-w-0 flex-1 truncate', focused && 'text-muted-foreground')}>
            {title}
            {renderer?.kind === 'dom' && (
              <span className="ml-2 opacity-60" title={renderer.reason}>
                (DOM renderer)
              </span>
            )}
          </span>
          {actions}
        </div>
      ) : (
        <div
          className={cn(
            'absolute right-3 z-10 flex items-center rounded-md bg-background/80',
            // Below the history view (h-[45%]) when it is open, so the two headers don't collide.
            historyOpen && controller ? 'top-[calc(45%+6px)]' : 'top-1.5'
          )}
        >
          {renderer?.kind === 'dom' && (
            <span
              className="mr-1 font-mono text-2xs text-dim opacity-0 group-hover/pane:opacity-100"
              title={renderer.reason}
            >
              DOM renderer
            </span>
          )}
          {actions}
        </div>
      )}
      {historyOpen && controller && (
        <HistoryView controller={controller} gaps={gaps} onClose={() => setHistoryOpen(false)} />
      )}
      <div ref={containerRef} className="terminal-host relative min-h-0 flex-1" />
      {exited && (
        <div className="pointer-events-none absolute right-3 bottom-2 rounded-[5px] border border-border bg-chrome px-2 py-0.5 font-mono text-2xs text-muted-foreground">
          [process exited{info?.exitCode != null ? ` with code ${info.exitCode}` : ''}]
        </div>
      )}
    </div>
  )
}

function splitFocusedFrom(hostId: string, pane: PaneId, dir: 'row' | 'column') {
  focusPane(hostId, pane)
  return splitFocused(dir)
}

function PaneButton({
  label,
  onClick,
  children
}: {
  label: string
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <IconButton
      label={label}
      side="bottom"
      className="size-5 rounded-[4px] text-quiet hover:bg-hover-strong hover:text-foreground dark:hover:bg-hover-strong"
      // Keep the click from refocusing the pane (and its xterm) first.
      onMouseDown={(e) => e.stopPropagation()}
      onClick={onClick}
    >
      {children}
    </IconButton>
  )
}
