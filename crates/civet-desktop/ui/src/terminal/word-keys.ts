// Option+Left / Option+Right move by word on macOS. xterm.js 6 sends them as
// CSI 1;3 D / CSI 1;3 C, which zsh and bash leave unbound, so they are sent as
// ESC b / ESC f (Meta+B / Meta+F, backward-word / forward-word), as
// Terminal.app and iTerm2 do. Installed in the pane's custom key handler.

export type WordKeyEvent = Pick<
  KeyboardEvent,
  'type' | 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
> & {
  preventDefault(): void
}

/** Sent for Option+Left. */
export const WORD_LEFT_SEQUENCE = '\x1bb'
/** Sent for Option+Right. */
export const WORD_RIGHT_SEQUENCE = '\x1bf'

/**
 * Returns false when the event was consumed (xterm then ignores it), true to
 * let xterm handle it. `send` is called once, on keydown.
 */
export function handleWordKey(
  ev: WordKeyEvent,
  isMac: boolean,
  send: (data: string) => void
): boolean {
  if (!isMac || !ev.altKey || ev.metaKey || ev.ctrlKey || ev.shiftKey) return true
  const seq =
    ev.key === 'ArrowLeft'
      ? WORD_LEFT_SEQUENCE
      : ev.key === 'ArrowRight'
        ? WORD_RIGHT_SEQUENCE
        : null
  if (!seq) return true
  if (ev.type === 'keydown') {
    ev.preventDefault()
    send(seq)
  }
  return false
}
