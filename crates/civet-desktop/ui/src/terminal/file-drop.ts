// What a file dropped on a pane turns into, ported from Orca
// (terminal-drop-image-path.ts, terminal-drop-path-writer.ts, posix only).
// Image files go in as a *bracketed paste* of the path: that is how terminal
// TUIs (Claude Code, Codex) recognise an image attachment, the same as a
// screenshot paste. Other files are typed as shell-escaped paths, like
// dropping a file on Ghostty or Terminal.app.

export type DropChunk = { kind: 'paste' | 'type'; text: string }

const IMAGE_EXTENSIONS = new Set(['.png', '.jpg', '.jpeg', '.gif', '.svg', '.webp', '.bmp', '.ico'])

// Why (Orca): a space is unsafe raw because Codex shlex-splits a pasted path.
const RAW_PASTE_UNSAFE_RE = /[ "'`$;&|<>(){}[\]*?!#\\]/

export function isImagePath(path: string): boolean {
  const dot = path.lastIndexOf('.')
  if (dot === -1 || dot < path.lastIndexOf('/')) return false
  return IMAGE_EXTENSIONS.has(path.slice(dot).toLowerCase())
}

function hasControlByte(path: string): boolean {
  for (let i = 0; i < path.length; i++) {
    const c = path.charCodeAt(i)
    if (c < 0x20 || c === 0x7f) return true
  }
  return false
}

/** Shell-escape for typed input: bare when safe, else single-quoted. */
export function shellEscapePath(path: string): string {
  if (/^[a-zA-Z0-9_./@:-]+$/.test(path)) return path
  return `'${path.replace(/'/g, "'\\''")}'`
}

function canPasteRaw(path: string): boolean {
  return !hasControlByte(path) && !RAW_PASTE_UNSAFE_RE.test(path)
}

/**
 * Text to bracketed-paste for an image path, or null when a paste frame would
 * alter it (control bytes). Why backslashes (Orca): Claude Code strips only one
 * outer quote pair, so a `'\''` splice would leak quotes into the path.
 */
function imagePasteText(path: string): string | null {
  if (hasControlByte(path)) return null
  if (canPasteRaw(path)) return path
  return path.replace(/[^a-zA-Z0-9_./@:\-\u0080-￿]/g, '\\$&')
}

/** The input to send, in order, for `paths` dropped on a pane. */
export function dropChunks(paths: readonly string[]): DropChunk[] {
  const out: DropChunk[] = []
  paths.forEach((path, i) => {
    const paste = isImagePath(path) ? imagePasteText(path) : null
    if (paste === null) {
      out.push({ kind: 'type', text: `${shellEscapePath(path)} ` })
      return
    }
    out.push({ kind: 'paste', text: paste })
    // A pasted image carries no trailing space; separate it from what follows
    // unless both are raw image pastes (self-delimiting for TUIs).
    const next = paths[i + 1]
    if (next !== undefined && !(paste === path && isImagePath(next) && canPasteRaw(next))) {
      out.push({ kind: 'type', text: ' ' })
    }
  })
  return out
}

type ClipboardData = Pick<DataTransfer, 'getData'> & {
  items: ArrayLike<Pick<DataTransferItem, 'kind' | 'type' | 'getAsFile'>>
}

/**
 * The image in a paste with no text (a screenshot), else null: text pastes
 * stay with xterm. Why (Orca terminal-clipboard-paste.ts): an agent on a
 * remote host cannot read this Mac's clipboard, so the image is uploaded and
 * its path pasted instead.
 */
export function clipboardImage(data: ClipboardData | null): File | null {
  if (!data || data.getData('text/plain')) return null
  for (let i = 0; i < data.items.length; i++) {
    const item = data.items[i]!
    if (item.kind === 'file' && item.type.startsWith('image/')) return item.getAsFile()
  }
  return null
}

/** `20250102-030405` (UTC) for file names. */
function fileTime(now: Date): string {
  return now.toISOString().replace(/[-:]/g, '').replace(/\..*/, '').replace('T', '-')
}

/** File name for a pasted image: `clipboard-<time>.<ext>`. */
export function clipboardImageName(type: string, now = new Date()): string {
  const ext =
    (type.split('/')[1] ?? 'png').replace('jpeg', 'jpg').replace(/[^a-z0-9]/g, '') || 'png'
  return `clipboard-${fileTime(now)}.${ext}`
}

/** File name for Paste as File (⇧⌘V): `paste-<time>.txt`. */
export function pasteFileName(now = new Date()): string {
  return `paste-${fileTime(now)}.txt`
}

export const LARGE_PASTE_LINES = 200
export const LARGE_PASTE_CHARS = 20 * 1024

/** A text paste big enough to suggest Paste as File instead (≈200 lines or ≈20 KB). */
export function isLargePaste(text: string): boolean {
  if (text.length >= LARGE_PASTE_CHARS) return true
  let lines = 1
  for (let i = text.indexOf('\n'); i !== -1; i = text.indexOf('\n', i + 1)) {
    if (++lines >= LARGE_PASTE_LINES) return true
  }
  return false
}
