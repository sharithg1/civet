// File paths in a line of terminal output: what an agent prints when it
// names a file ("Saved the report to /tmp/report.pdf", "src/app.ts:12").
// Only candidates: the terminal checks each one against the host before it
// becomes a link (see terminal/file-links.ts), so prose that merely looks
// like a path ("and/or", "e.g") never does.

/** A possible file path in a line. */
export type PathCandidate = {
  /** Start in the line (UTF-16 index) … */
  start: number
  /** … and end (exclusive), `:line:col` suffix included. */
  end: number
  /** The path as printed: absolute, `~/…` or relative. */
  path: string
  /** Line number from a `:12` / `:12:5` suffix. */
  line: number | null
}

/** At most this many candidates per line (each costs a request to the host). */
export const MAX_CANDIDATES_PER_LINE = 6

// A run of characters a path is usually printed with (no spaces, quotes or
// brackets), then an optional `:line` / `:line:col`.
const TOKEN = /[\p{L}\p{N}_.~/+@%-]+(?::\d+(?::\d+)?)?/gu
/** `name.ext`, the extension starting with a letter (not `0.6.1`). */
const BARE_FILE = /^[^/]*[^/.]\.[A-Za-z][A-Za-z0-9]{0,9}$/

function looksLikePath(p: string): boolean {
  if (p.startsWith('//')) return false
  // Something besides separators and dots: not "/", "..", "~/".
  if (!/[^/.~]/.test(p)) return false
  if (p.includes('/')) return true
  return BARE_FILE.test(p)
}

/** The file path candidates in `text`, left to right. */
export function findPathCandidates(text: string): PathCandidate[] {
  const out: PathCandidate[] = []
  for (const m of text.matchAll(TOKEN)) {
    if (out.length >= MAX_CANDIDATES_PER_LINE) break
    const start = m.index
    // The rest of a URL ("https" ":" "//host/a.html") or of an scp-style
    // remote ("host:/path").
    if (text[start - 1] === ':') continue
    let token = m[0]
    let line: number | null = null
    const suffix = /:(\d+)(?::\d+)?$/.exec(token)
    if (suffix) {
      line = Number.parseInt(suffix[1]!, 10)
      token = token.slice(0, suffix.index)
    }
    let end = start + m[0].length
    if (!suffix) {
      // The end of a sentence is not part of the name.
      const trimmed = token.replace(/\.+$/, '')
      end -= token.length - trimmed.length
      token = trimmed
    }
    if (!looksLikePath(token)) continue
    // An email address or user@host.
    if (!token.includes('/') && token.includes('@')) continue
    out.push({ start, end, path: token, line })
  }
  return out
}

/** `path` relative to the directory `root`, or null when it is not inside it. */
export function relativeTo(root: string, path: string): string | null {
  const base = root.endsWith('/') ? root : `${root}/`
  return path.startsWith(base) && path.length > base.length ? path.slice(base.length) : null
}

/** "1.2 MB", "340 KB", "12 B". */
export function formatFileSize(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let n = bytes
  let u = 0
  while (n >= 1000 && u < units.length - 1) {
    n /= 1000
    u++
  }
  return `${u === 0 || n >= 100 ? Math.round(n) : n.toFixed(1)} ${units[u]}`
}

/** An `http(s)://` URL in a line. */
export type UrlMatch = {
  /** Start in the line (UTF-16 index) … */
  start: number
  /** … and end (exclusive). */
  end: number
  url: string
}

// Up to whitespace, quotes, angle brackets or a box-drawing border.
const URL = /https?:\/\/[^\s"'`<>\u2500-\u257f]+/g
const CLOSERS: Record<string, string> = { ')': '(', ']': '[', '}': '{' }

/** The web URLs in `text`, without the punctuation of the sentence around them. */
export function findUrls(text: string): UrlMatch[] {
  const out: UrlMatch[] = []
  for (const m of text.matchAll(URL)) {
    let url = m[0]
    for (;;) {
      const last = url[url.length - 1]!
      const opener = CLOSERS[last]
      // "(see https://a.b/c)" ends before the bracket; ".../Rust_(language)" keeps its own.
      const unbalanced = opener && url.split(opener).length < url.split(last).length
      if (!unbalanced && !'.,;:!?*'.includes(last)) break
      url = url.slice(0, -1)
    }
    // Something after the scheme.
    if (!/^https?:\/\/[^/?#]/.test(url)) continue
    out.push({ start: m.index, end: m.index + url.length, url })
  }
  return out
}
