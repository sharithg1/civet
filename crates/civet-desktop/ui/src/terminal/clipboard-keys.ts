// macOS clipboard shortcuts for an xterm.js pane, installed with
// term.attachCustomKeyEventHandler. WKWebView only fires the DOM copy event
// xterm.js listens for when WebKit itself thinks there is something to copy
// (xterm's selection lives in a canvas, not the DOM), so Cmd+C is handled
// explicitly and written through the Rust side. Cmd+V is left alone: the app
// menu's Paste fires a real paste event on xterm's textarea, and xterm.js
// applies bracketed paste mode itself. Ctrl+C still goes to the pty.

export type ClipboardKeyEvent = Pick<
  KeyboardEvent,
  'type' | 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
> & { preventDefault(): void }

export type ClipboardKeyTerminal = {
  hasSelection(): boolean
  getSelection(): string
  selectAll(): void
}

export type ClipboardKeyOptions = {
  isMac: boolean
  copy: (text: string) => void
}

/** True on macOS (Cmd shortcuts). */
export function isMacPlatform(nav: { platform?: string; userAgent?: string } | undefined = globalThis.navigator): boolean {
  if (!nav) return false
  return /Mac|iPhone|iPad/.test(nav.platform || '') || /Macintosh/.test(nav.userAgent || '')
}

/**
 * xterm custom key handler: returns false when the event was consumed (xterm
 * then ignores it), true to let xterm and the browser handle it as usual.
 */
export function handleClipboardKey(
  ev: ClipboardKeyEvent,
  term: ClipboardKeyTerminal,
  opts: ClipboardKeyOptions
): boolean {
  if (!opts.isMac || !ev.metaKey || ev.ctrlKey || ev.altKey || ev.shiftKey) return true
  const key = ev.key.toLowerCase()
  if (key === 'c') {
    // With no selection Cmd+C does nothing (it is not ^C on macOS).
    if (ev.type === 'keydown' && term.hasSelection()) {
      const text = term.getSelection()
      if (text) opts.copy(text)
    }
    ev.preventDefault()
    return false
  }
  if (key === 'a') {
    if (ev.type === 'keydown') term.selectAll()
    // Keep WebKit from selecting the whole page.
    ev.preventDefault()
    return false
  }
  return true
}
