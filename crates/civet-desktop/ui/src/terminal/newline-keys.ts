// Shift+Enter inserts a newline instead of submitting. xterm.js sends a bare
// CR for Shift+Enter (same as Enter), so it is sent as ESC CR (Meta+Enter),
// which Claude Code reads as "newline" (what its /terminal-setup configures
// in VS Code) and zsh's line editor inserts as a literal newline. Installed
// in the pane's custom key handler, before the clipboard keys.

export type NewlineKeyEvent = Pick<
  KeyboardEvent,
  'type' | 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
> & {
  preventDefault(): void
}

/** Sent for Shift+Enter. */
export const SHIFT_ENTER_SEQUENCE = '\x1b\r'

/**
 * Returns false when the event was consumed (xterm then ignores it), true to
 * let xterm handle it. `send` is called once, on keydown.
 */
export function handleShiftEnter(ev: NewlineKeyEvent, send: (data: string) => void): boolean {
  if (ev.key !== 'Enter' || !ev.shiftKey || ev.metaKey || ev.ctrlKey || ev.altKey) return true
  if (ev.type === 'keydown') {
    ev.preventDefault()
    send(SHIFT_ENTER_SEQUENCE)
  }
  // keypress / keyup of the same key: swallow too, or xterm would send CR.
  return false
}
