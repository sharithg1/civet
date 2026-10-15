// Returning focus to the terminal after a dialog closes. Radix dialogs restore
// focus to whatever was focused when they opened (often a sidebar button, or
// nothing when opened from the store); Civet's dialogs are opened from the
// store, so the useful place to land is the active tab's focused pane.
import { useStore, wsKey } from '@/state/store'

/** Focuses the active tab's focused xterm. Returns false when there is none. */
export function focusActiveTerminal(): boolean {
  const { active, workspaces } = useStore.getState()
  if (!active) return false
  const ws = workspaces[wsKey(active.hostId, active.worktree)]
  const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
  if (!tab) return false
  // `>`: the pane's own .terminal-host, not the read-only HistoryView xterm.
  const textarea = document.querySelector<HTMLTextAreaElement>(
    `[data-pane-id="${tab.focused}"] > .terminal-host .xterm-helper-textarea`
  )
  if (!textarea) return false
  textarea.focus({ preventScroll: true })
  return true
}

/** `onCloseAutoFocus` handler for dialog content: prefer the terminal over Radix's default. */
export function restoreTerminalFocus(event: Event): void {
  if (focusActiveTerminal()) event.preventDefault()
}
