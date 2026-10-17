// Find-in-terminal shortcuts. In the pane (xterm's custom key handler) Cmd+F
// opens the find bar and Cmd+G / Shift+Cmd+G step through matches; elsewhere
// Ctrl+Shift+F opens it (plain Ctrl+F is forward-char in shells). In the find
// bar's input Enter / Shift+Enter step, Esc closes. Matching runs in
// @xterm/addon-search over the lines loaded in xterm's buffer.

export type FindKeyEvent = Pick<
  KeyboardEvent,
  'type' | 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'
> & { isComposing?: boolean; preventDefault(): void }

export type FindAction = 'open' | 'next' | 'previous'

export type FindInputAction = FindAction | 'close'

export type FindOptions = {
  caseSensitive: boolean
  regex: boolean
  wholeWord: boolean
}

/** The pane-level find shortcut in `ev`, if any (keydown or not). */
export function findShortcut(ev: FindKeyEvent, isMac: boolean): FindAction | null {
  if (ev.altKey) return null
  const key = ev.key.toLowerCase()
  if (isMac) {
    if (!ev.metaKey || ev.ctrlKey) return null
    if (key === 'f' && !ev.shiftKey) return 'open'
    if (key === 'g') return ev.shiftKey ? 'previous' : 'next'
    return null
  }
  if (ev.ctrlKey && ev.shiftKey && !ev.metaKey && key === 'f') return 'open'
  return null
}

/**
 * xterm custom key handler: returns false when the event was consumed (xterm
 * then ignores it), true to let xterm and the browser handle it as usual.
 */
export function handleFindKey(
  ev: FindKeyEvent,
  opts: { isMac: boolean; onFind: (action: FindAction) => void }
): boolean {
  const action = findShortcut(ev, opts.isMac)
  if (!action) return true
  if (ev.type === 'keydown') opts.onFind(action)
  ev.preventDefault()
  return false
}

/** What a keydown in the find bar's input does. */
export function findInputAction(ev: FindKeyEvent, isMac: boolean): FindInputAction | null {
  if (ev.isComposing) return null
  if (ev.key === 'Escape') return 'close'
  if (ev.key === 'Enter' && !ev.metaKey && !ev.ctrlKey && !ev.altKey) {
    return ev.shiftKey ? 'previous' : 'next'
  }
  return findShortcut(ev, isMac)
}

/** Why `query` cannot be searched (an invalid regex), or null. */
export function findQueryError(query: string, opts: Pick<FindOptions, 'regex'>): string | null {
  if (!opts.regex || !query) return null
  try {
    new RegExp(query)
    return null
  } catch (err) {
    return (err as Error).message
  }
}

/** The find bar's counter: "3 of 12", "No results", "1000+ matches". */
export function findResultLabel(
  query: string,
  results: { resultIndex: number; resultCount: number } | null,
  limit: number
): string {
  if (!query || !results) return ''
  const { resultIndex, resultCount } = results
  if (resultCount === 0) return 'No results'
  if (resultCount >= limit && resultIndex < 0) return `${limit}+ matches`
  if (resultIndex < 0) return `${resultCount} ${resultCount === 1 ? 'match' : 'matches'}`
  return `${resultIndex + 1} of ${resultCount}${resultCount >= limit ? '+' : ''}`
}
