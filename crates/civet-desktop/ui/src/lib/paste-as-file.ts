// Edit → Paste as File (⇧⌘V), from the app menu (civet://paste-as-file):
// the terminal pane holding keyboard focus saves the clipboard text as a
// file on its host and gets the path typed in. With no terminal focused
// (an editor, a dialog, the sidebar) it only says so.

import { useEffect } from 'react'
import { pushToast, useStore } from '@/state/store'
import { paneController } from '@/terminal/pane-registry'

function focusedPaneElement(): HTMLElement | null {
  return document.activeElement?.closest<HTMLElement>('[data-pane-id][data-host-id]') ?? null
}

export function usePasteAsFile(): void {
  const backend = useStore((s) => s.backend)
  useEffect(() => {
    if (!backend) return
    let unlisten: (() => void) | undefined
    let disposed = false
    void backend
      .onPasteAsFile(() => {
        const el = focusedPaneElement()
        const c = el && paneController(el.dataset.hostId!, Number(el.dataset.paneId))
        if (!c) {
          pushToast({ kind: 'info', title: 'Paste as File works in a terminal pane' }, 3200)
          return
        }
        void c.pasteAsFile()
      })
      .then((u) => {
        if (disposed) u()
        else unlisten = u
      })
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [backend])
}
