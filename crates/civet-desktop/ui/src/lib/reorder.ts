// Drag-to-reorder for a vertical list (sidebar hosts and projects), with
// pointer events rather than HTML5 drag and drop, which Tauri's webview
// file-drop handling can swallow. The list element holds the items as direct
// children marked `data-reorder-id`; a handle (the item's header row) starts
// a drag once the pointer moved 4px, so a plain click still works (e.g.
// collapse), and the click that ends a drag is swallowed.

import { useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'

const THRESHOLD = 4

/** `ids` with `id` moved to insertion index `to` (an index into `ids`, before removal). */
export function moveTo(ids: readonly string[], id: string, to: number): string[] {
  const from = ids.indexOf(id)
  if (from < 0) return [...ids]
  const next = ids.filter((x) => x !== id)
  next.splice(to > from ? to - 1 : to, 0, id)
  return next
}

export type Reorder = {
  /** Ref for the list element. */
  listRef: React.RefObject<HTMLDivElement | null>
  /** Id being dragged, or null. */
  dragging: string | null
  /** Drop line for item `i`: above it, below it (last item only) or none. */
  indicator: (i: number) => 'top' | 'bottom' | null
  /** `onPointerDown` for an item's drag handle. */
  handle: (id: string) => (e: ReactPointerEvent<HTMLElement>) => void
}

export function useReorder(ids: readonly string[], onReorder: (ids: string[]) => void): Reorder {
  const listRef = useRef<HTMLDivElement>(null)
  const [drag, setDrag] = useState<{ id: string; to: number } | null>(null)

  const handle = (id: string) => (e: ReactPointerEvent<HTMLElement>) => {
    // Buttons inside the header (menus, "+") keep their clicks.
    if (e.button !== 0 || (e.target as HTMLElement).closest('button, input, a')) return
    const startY = e.clientY
    let active = false
    let to = -1
    const targetIndex = (y: number): number => {
      const items =
        listRef.current?.querySelectorAll<HTMLElement>(':scope > [data-reorder-id]') ?? []
      for (let i = 0; i < items.length; i++) {
        const r = items[i]!.getBoundingClientRect()
        if (y < r.top + r.height / 2) return i
      }
      return items.length
    }
    const move = (ev: PointerEvent) => {
      if (!active) {
        if (Math.abs(ev.clientY - startY) < THRESHOLD) return
        active = true
        document.body.style.userSelect = 'none'
        document.body.style.cursor = 'grabbing'
      }
      to = targetIndex(ev.clientY)
      setDrag({ id, to })
    }
    const end = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', end)
      window.removeEventListener('pointercancel', end)
      setDrag(null)
      if (!active) return
      document.body.style.userSelect = ''
      document.body.style.cursor = ''
      // The click that follows pointerup must not toggle / select.
      const swallow = (ev: MouseEvent) => {
        ev.stopPropagation()
        ev.preventDefault()
      }
      window.addEventListener('click', swallow, { capture: true, once: true })
      setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 0)
      const next = moveTo(ids, id, to)
      if (next.some((x, i) => x !== ids[i])) onReorder(next)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', end)
    window.addEventListener('pointercancel', end)
  }

  const indicator = (i: number): 'top' | 'bottom' | null => {
    if (!drag) return null
    const from = ids.indexOf(drag.id)
    // Dropping right where the item already is: no line.
    if (drag.to === from || drag.to === from + 1) return null
    if (drag.to === i) return 'top'
    if (drag.to === ids.length && i === ids.length - 1) return 'bottom'
    return null
  }

  return { listRef, dragging: drag?.id ?? null, indicator, handle }
}

/** Absolutely positioned drop line classes for an item with `position: relative`. */
export function dropLineClass(where: 'top' | 'bottom' | null): string {
  if (!where) return ''
  return where === 'top'
    ? 'before:pointer-events-none before:absolute before:inset-x-2 before:-top-px before:h-0.5 before:rounded-full before:bg-ring'
    : 'after:pointer-events-none after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-ring'
}
