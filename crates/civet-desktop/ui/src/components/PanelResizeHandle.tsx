// Drag handle on a side panel's inner edge (the sidebar's right edge, the
// right panel's left edge). The panel keeps the width while dragging and
// saves it once, on release; double-click restores the default.
import { useState, type PointerEvent } from 'react'
import { cn } from '@/lib/utils'

export function PanelResizeHandle({
  edge,
  label,
  width,
  clamp,
  onDrag,
  onCommit,
  onReset
}: {
  /** Which edge of the panel it sits on; dragging away from the panel widens it. */
  edge: 'left' | 'right'
  label: string
  width: number
  clamp: (px: number) => number
  /** The width while dragging, null when the drag ends. */
  onDrag: (w: number | null) => void
  onCommit: (w: number) => void
  onReset: () => void
}) {
  const [start, setStart] = useState<{ x: number; width: number } | null>(null)
  const at = (e: PointerEvent) => {
    const dx = e.clientX - start!.x
    return clamp(start!.width + (edge === 'right' ? dx : -dx))
  }
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={width}
      title="Drag to resize, double-click to reset"
      onPointerDown={(e) => {
        if (e.button !== 0) return
        e.preventDefault()
        e.currentTarget.setPointerCapture(e.pointerId)
        setStart({ x: e.clientX, width })
      }}
      onPointerMove={(e) => {
        if (start) onDrag(at(e))
      }}
      onPointerUp={(e) => {
        if (!start) return
        onCommit(at(e))
        setStart(null)
        onDrag(null)
      }}
      onPointerCancel={() => {
        setStart(null)
        onDrag(null)
      }}
      onDoubleClick={onReset}
      className={cn(
        'absolute inset-y-0 z-10 w-1.5 cursor-col-resize transition-colors hover:bg-border-strong',
        edge === 'right' ? '-right-[3px]' : '-left-[3px]',
        start && 'bg-border-strong'
      )}
    />
  )
}
