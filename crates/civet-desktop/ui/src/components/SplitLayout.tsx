// Renders a tab's pane split tree. Divider look and hit-area behaviour are
// ported from Orca: src/renderer/src/lib/pane-manager/pane-divider.ts and
// assets/terminal.css (.pane-divider); Orca builds DOM imperatively, this is
// the declarative equivalent over layout/pane-tree.ts.

import { useRef } from 'react'
import type { NodePath, PaneNode } from '../layout/pane-tree'
import { setSplitRatio } from '../state/store'
import { TerminalPane } from './TerminalPane'

type Props = {
  hostId: string
  wsKey: string
  tabId: string
  node: PaneNode
  focused: number
  multiPane: boolean
  path?: NodePath
}

const DIVIDER_HIT_PX = 6

export function SplitLayout({ hostId, wsKey, tabId, node, focused, multiPane, path = [] }: Props) {
  const ref = useRef<HTMLDivElement>(null)
  if (node.type === 'leaf') {
    return (
      <TerminalPane
        key={node.pane}
        hostId={hostId}
        pane={node.pane}
        focused={node.pane === focused}
        multiPane={multiPane}
      />
    )
  }
  const vertical = node.dir === 'row' // side by side → vertical divider line
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const container = ref.current
    if (!container) return
    e.preventDefault()
    const divider = e.currentTarget
    divider.setPointerCapture(e.pointerId)
    divider.classList.add('is-dragging')
    const rect = container.getBoundingClientRect()
    const move = (ev: PointerEvent) => {
      const ratio = vertical
        ? (ev.clientX - rect.left) / rect.width
        : (ev.clientY - rect.top) / rect.height
      setSplitRatio(wsKey, tabId, path, ratio)
    }
    const up = () => {
      divider.classList.remove('is-dragging')
      divider.removeEventListener('pointermove', move)
      divider.removeEventListener('pointerup', up)
      divider.removeEventListener('pointercancel', up)
    }
    divider.addEventListener('pointermove', move)
    divider.addEventListener('pointerup', up)
    divider.addEventListener('pointercancel', up)
  }
  const common = { hostId, wsKey, tabId, focused, multiPane }
  return (
    <div
      ref={ref}
      className={`flex h-full w-full min-h-0 min-w-0 ${vertical ? 'flex-row' : 'flex-col'}`}
    >
      <div className="min-h-0 min-w-0" style={{ flex: `${node.ratio} 1 0` }}>
        <SplitLayout {...common} node={node.a} path={[...path, 'a']} />
      </div>
      <div
        className={`pane-divider ${vertical ? 'is-vertical' : 'is-horizontal'}`}
        style={{
          flex: 'none',
          position: 'relative',
          cursor: vertical ? 'col-resize' : 'row-resize',
          [vertical ? 'width' : 'height']: `${DIVIDER_HIT_PX}px`
        }}
        role="separator"
        aria-orientation={vertical ? 'vertical' : 'horizontal'}
        onPointerDown={onPointerDown}
      />
      <div className="min-h-0 min-w-0" style={{ flex: `${1 - node.ratio} 1 0` }}>
        <SplitLayout {...common} node={node.b} path={[...path, 'b']} />
      </div>
    </div>
  )
}
