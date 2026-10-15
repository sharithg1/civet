// One terminal leaf in a split layout. Pane chrome (focus rim, exited state,
// title strip) follows Orca's src/renderer/src/components/terminal-pane/
// (TerminalPane.tsx + assets/terminal.css); the PTY/IPC plumbing is replaced
// by PaneController (attach_pane binary channel).

import { useEffect, useRef, useState } from 'react'
import { Columns2, History, Rows2, X } from 'lucide-react'
import { useStore, closePanes, focusPane, splitFocused, reportError } from '@/state/store'
import { PaneController } from '@/terminal/pane-controller'
import type { LineRange } from '@/terminal/pane-stream'
import type { PaneId } from '@/types'
import { cn } from '@/lib/utils'
import { indicatorFor } from '@/lib/agent-status'
import { HistoryView } from './HistoryView'
import { AgentStateDot } from './AgentStateDot'
import { IconButton } from './IconButton'

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
      onError: (message) => reportError(`Pane ${pane}`, message)
    })
    c.mount(el)
    setController(c)
    return () => {
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

  return (
    <div
      className={cn(
        'group/pane relative flex h-full w-full min-w-0 flex-col overflow-hidden bg-[#282c34]',
        multiPane && (focused ? 'pane-focused' : 'pane-unfocused')
      )}
      onMouseDown={() => focusPane(hostId, pane)}
      data-pane-id={pane}
    >
      <div className="flex h-6 shrink-0 items-center gap-1.5 px-2 text-[11px] text-terminal-pane-title-on-dark-fg">
        {info?.agent && <AgentStateDot state={indicatorFor(info.status)} />}
        <span className="min-w-0 flex-1 truncate">
          {info?.title || info?.command.join(' ') || `pane ${pane}`}
          {renderer?.kind === 'dom' && (
            <span className="ml-2 opacity-60" title={renderer.reason}>
              (DOM renderer)
            </span>
          )}
        </span>
        <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover/pane:opacity-100">
          <PaneButton label="Show older history" onClick={() => setHistoryOpen((v) => !v)}>
            <History className="size-3" />
          </PaneButton>
          <PaneButton
            label="Split right"
            onClick={() => void splitFocusedFrom(hostId, pane, 'row')}
          >
            <Columns2 className="size-3" />
          </PaneButton>
          <PaneButton
            label="Split down"
            onClick={() => void splitFocusedFrom(hostId, pane, 'column')}
          >
            <Rows2 className="size-3" />
          </PaneButton>
          <PaneButton label="Close pane" onClick={() => closePanes(hostId, [pane])}>
            <X className="size-3" />
          </PaneButton>
        </div>
      </div>
      {historyOpen && controller && (
        <HistoryView controller={controller} gaps={gaps} onClose={() => setHistoryOpen(false)} />
      )}
      <div ref={containerRef} className="terminal-host relative min-h-0 flex-1 pl-1" />
      {exited && (
        <div className="pointer-events-none absolute bottom-2 right-3 rounded bg-black/60 px-2 py-0.5 text-[11px] text-white/80">
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
      className="size-5 rounded-sm text-terminal-pane-title-on-dark-button-fg/70 hover:bg-white/10 hover:text-terminal-pane-title-on-dark-button-hover-fg dark:hover:bg-white/10"
      // Keep the click from refocusing the pane (and its xterm) first.
      onMouseDown={(e) => e.stopPropagation()}
      onClick={onClick}
    >
      {children}
    </IconButton>
  )
}
