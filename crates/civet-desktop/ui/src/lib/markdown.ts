// Helpers for the Markdown preview of file tabs (MarkdownView.tsx): which
// files get one, where a link in them points, and heading anchors.

const MARKDOWN_EXT = /\.(md|markdown|mdown|mkd)$/i

export function isMarkdownPath(path: string): boolean {
  return MARKDOWN_EXT.test(path)
}

export type MarkdownLink =
  /** `http(s)://`: opened in the browser. */
  | { kind: 'web'; url: string }
  /** Another file of the worktree (worktree-relative), maybe with a `#heading`. */
  | { kind: 'file'; path: string; hash: string }
  /** `#heading` in this file. */
  | { kind: 'anchor'; id: string }

/**
 * Where `href` in the Markdown file at `from` (worktree-relative) points;
 * null for links that go nowhere openable (other schemes, outside the
 * worktree).
 */
export function resolveLink(from: string, href: string): MarkdownLink | null {
  const h = href.trim()
  if (!h) return null
  if (/^https?:\/\//.test(h)) return { kind: 'web', url: h }
  if (h.startsWith('#')) return h.length > 1 ? { kind: 'anchor', id: decode(h.slice(1)) } : null
  // Any other scheme (`file:`, `javascript:`, `vscode:` …) or protocol-relative.
  if (/^[a-z][a-z0-9+.-]*:/i.test(h) || h.startsWith('//')) return null
  const hashAt = h.indexOf('#')
  const target = (hashAt < 0 ? h : h.slice(0, hashAt)).split('?')[0] ?? ''
  const hash = hashAt < 0 ? '' : decode(h.slice(hashAt + 1))
  // `/x` is relative to the worktree root (as on GitHub), anything else to the file.
  const base = target.startsWith('/') ? [] : from.split('/').slice(0, -1)
  const parts = [...base]
  for (const seg of decode(target).split('/')) {
    if (seg === '' || seg === '.') continue
    if (seg === '..') {
      if (!parts.length) return null
      parts.pop()
    } else parts.push(seg)
  }
  if (!parts.length) return null
  return { kind: 'file', path: parts.join('/'), hash }
}

function decode(s: string): string {
  try {
    return decodeURIComponent(s)
  } catch {
    return s
  }
}

/** GitHub-style heading id: lowercase, punctuation dropped, spaces to `-`. */
export function slugify(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, '')
    .replace(/\s/g, '-')
}

/**
 * Ids for a document's headings in order, de-duplicated like GitHub (`x`,
 * `x-1`, …), skipping suffixes another heading already took (`A-1`, `A`, `A`
 * → `a-1`, `a`, `a-2`).
 */
export class Slugger {
  private used = new Set<string>()

  slug(text: string): string {
    const base = slugify(text)
    let id = base
    for (let n = 1; this.used.has(id); n++) id = `${base}-${n}`
    this.used.add(id)
    return id
  }
}

/** `data:` URLs of raster images, the only images the preview shows. */
export function isDataImage(url: string): boolean {
  return /^data:image\/(png|jpe?g|gif|webp|avif)[;,]/i.test(url)
}
