// Unified diff text (`git diff` output for one file) → hunks of numbered
// lines for the Changes tab. Combined diffs of a conflicted file
// (`diff --cc`, `@@@ … @@@`, one prefix column per parent) keep only
// new-side line numbers.

export type DiffLineKind = 'context' | 'add' | 'del' | 'note'

export type DiffLine = {
  kind: DiffLineKind
  /** Text without the prefix column(s). */
  text: string
  oldNo: number | null
  newNo: number | null
}

export type DiffHunk = {
  /** The `@@ … @@` line. */
  header: string
  lines: DiffLine[]
}

export type ParsedDiff = {
  /** `diff --git`, `index`, `rename from`, `---`/`+++`, … lines before the first hunk. */
  headers: string[]
  hunks: DiffHunk[]
  /** A combined diff (`diff --cc`) of an unmerged file. */
  combined: boolean
  /** Lines over all hunks. */
  lineCount: number
}

const HUNK = /^(@{2,}) (.*?) \1(.*)$/

export function parseUnifiedDiff(text: string): ParsedDiff {
  const out: ParsedDiff = { headers: [], hunks: [], combined: false, lineCount: 0 }
  const lines = text.split('\n')
  if (lines.at(-1) === '') lines.pop()
  let hunk: DiffHunk | null = null
  let parents = 1
  let oldNo = 0
  let newNo = 0
  for (const line of lines) {
    const m = HUNK.exec(line)
    if (m) {
      parents = m[1]!.length - 1
      out.combined ||= parents > 1
      const ranges = m[2]!.split(' ')
      oldNo = parents === 1 ? rangeStart(ranges[0]) : 0
      newNo = rangeStart(ranges.at(-1))
      hunk = { header: line, lines: [] }
      out.hunks.push(hunk)
      continue
    }
    if (!hunk) {
      out.headers.push(line)
      continue
    }
    if (line.startsWith('\\')) {
      hunk.lines.push({ kind: 'note', text: line.slice(2), oldNo: null, newNo: null })
      out.lineCount++
      continue
    }
    const prefix = line.slice(0, parents)
    const rest = line.slice(parents)
    let kind: DiffLineKind = 'context'
    if (prefix.includes('-')) kind = 'del'
    else if (prefix.includes('+')) kind = 'add'
    const single = parents === 1
    hunk.lines.push({
      kind,
      text: rest,
      oldNo: single && kind !== 'add' ? oldNo : null,
      newNo: kind !== 'del' ? newNo : null
    })
    if (single && kind !== 'add') oldNo++
    if (kind !== 'del') newNo++
    out.lineCount++
  }
  return out
}

/** `-12,5` / `+3` / `-0,0` → 12 / 3 / 0. */
function rangeStart(range: string | undefined): number {
  const n = Number.parseInt((range ?? '').slice(1).split(',')[0] ?? '', 10)
  return Number.isFinite(n) ? n : 0
}

/** `src/a/b.rs` → ['b.rs', 'src/a']. */
export function splitPath(path: string): [name: string, dir: string] {
  const i = path.lastIndexOf('/')
  return i < 0 ? [path, ''] : [path.slice(i + 1), path.slice(0, i)]
}

/**
 * A diff's two sides as code, for syntax highlighting: the old side (context
 * and deleted lines) and the new side (context and added lines), each one
 * line per row in hunk order, so a grammar sees continuous code rather than
 * single lines. `rows[h][i]` is where hunk h's line i landed: the row of its
 * side (context lines use the new side), or null for notes.
 */
export type DiffSides = {
  old: string
  new: string
  rows: ({ side: 'old' | 'new'; row: number } | null)[][]
}

export function diffSides(parsed: ParsedDiff): DiffSides {
  const old: string[] = []
  const neu: string[] = []
  const rows = parsed.hunks.map((h) =>
    h.lines.map((l) => {
      if (l.kind === 'note') return null
      if (l.kind === 'del') {
        old.push(l.text)
        return { side: 'old' as const, row: old.length - 1 }
      }
      if (l.kind === 'context') old.push(l.text)
      neu.push(l.text)
      return { side: 'new' as const, row: neu.length - 1 }
    })
  )
  return { old: old.join('\n'), new: neu.join('\n'), rows }
}
