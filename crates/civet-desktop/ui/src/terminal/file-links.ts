// File paths in a pane's output become links: hovering a line asks the
// pane's host which of its path-looking words are files (lib/terminal-paths
// finds the words), and clicking one hands it to the pane (a menu: open in
// Civet, open on this Mac, download and open from a remote host).

import type { IBufferLine, ILink, ILinkProvider, Terminal } from '@xterm/xterm'
import { findPathCandidates } from '../lib/terminal-paths'
import type { HostFile } from '../types'

/** A file named in a pane's output. */
export type FileLink = {
  /** The file on the pane's host. */
  file: HostFile
  /** Line number printed after it (`src/a.ts:12`). */
  line: number | null
}

/** How long an answer about a path is reused (hovering re-asks per mouse move). */
export const FILE_LINK_TTL_MS = 5000
const MAX_CACHED = 500

/**
 * Asks the host about a path once per `ttlMs`: the file, or null when
 * there is none (or the host can't say). Never rejects.
 */
export class FileResolver {
  private readonly cache = new Map<string, { at: number; file: Promise<HostFile | null> }>()

  constructor(
    private readonly stat: (path: string) => Promise<HostFile>,
    private readonly ttlMs = FILE_LINK_TTL_MS,
    private readonly now: () => number = Date.now
  ) {}

  resolve(path: string): Promise<HostFile | null> {
    const at = this.now()
    const hit = this.cache.get(path)
    if (hit && at - hit.at < this.ttlMs) return hit.file
    let file: Promise<HostFile | null>
    try {
      file = this.stat(path).catch(() => null)
    } catch {
      file = Promise.resolve(null)
    }
    this.cache.delete(path)
    this.cache.set(path, { at, file })
    // Oldest first out.
    if (this.cache.size > MAX_CACHED) this.cache.delete(this.cache.keys().next().value!)
    return file
  }
}

/**
 * A buffer line's text and, for each UTF-16 unit of it, the cell (0-based
 * column) it is drawn in: wide characters take two cells, so string
 * indices are not columns.
 */
export function lineCells(line: IBufferLine): { text: string; cols: number[] } {
  let text = ''
  const cols: number[] = []
  for (let x = 0; x < line.length; x++) {
    const cell = line.getCell(x)
    if (!cell || cell.getWidth() === 0) continue
    const chars = cell.getChars() || ' '
    text += chars
    for (let i = 0; i < chars.length; i++) cols.push(x)
  }
  return { text, cols }
}

/** Links for the files named in a line, checked against the host. */
export function fileLinkProvider(
  term: Terminal,
  resolver: FileResolver,
  onOpen: (event: MouseEvent, link: FileLink) => void
): ILinkProvider {
  return {
    provideLinks(y, callback) {
      const line = term.buffer.active.getLine(y - 1)
      if (!line) return callback(undefined)
      const { text, cols } = lineCells(line)
      const candidates = findPathCandidates(text)
      if (!candidates.length) return callback(undefined)
      void Promise.all(candidates.map((c) => resolver.resolve(c.path))).then((files) => {
        const links: ILink[] = []
        candidates.forEach((c, i) => {
          const file = files[i]
          if (!file) return
          links.push({
            range: {
              start: { x: cols[c.start]! + 1, y },
              end: { x: cols[c.end - 1]! + 1, y }
            },
            text: text.slice(c.start, c.end),
            activate: (event) => onOpen(event, { file, line: c.line })
          })
        })
        callback(links.length ? links : undefined)
      })
    }
  }
}
