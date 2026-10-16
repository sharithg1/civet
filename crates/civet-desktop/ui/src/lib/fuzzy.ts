// Quick-open matching for the Files tab: the query's characters must appear
// in the path in order (case-insensitive). Matches score higher when they
// are consecutive, start a path segment or word, or fall in the file name.

export type FuzzyMatch = { path: string; score: number; positions: number[] }

const WORD_START = /[/._\-\s]/

/** The best match of `query` in `path`, or null. Spaces in the query are ignored. */
export function fuzzyMatch(query: string, path: string): FuzzyMatch | null {
  const q = query.replace(/\s+/g, '').toLowerCase()
  if (!q) return { path, score: 0, positions: [] }
  const p = path.toLowerCase()
  const base = path.lastIndexOf('/') + 1
  // Greedy from the right end of the path: prefer matches in the file name.
  const positions: number[] = []
  let qi = q.length - 1
  for (let i = p.length - 1; i >= 0 && qi >= 0; i--) {
    if (p[i] === q[qi]) {
      positions.push(i)
      qi--
    }
  }
  if (qi >= 0) return null
  positions.reverse()
  let score = 0
  for (let k = 0; k < positions.length; k++) {
    const i = positions[k]!
    score += 1
    if (k > 0 && positions[k - 1] === i - 1) score += 4
    if (i === 0 || WORD_START.test(path[i - 1]!)) score += 3
    if (i >= base) score += 2
  }
  // Shorter paths win ties.
  score -= path.length / 100
  return { path, score, positions }
}

/** The `limit` best matches, best first. */
export function fuzzyFilter(query: string, paths: readonly string[], limit = 200): FuzzyMatch[] {
  const out: FuzzyMatch[] = []
  for (const path of paths) {
    const m = fuzzyMatch(query, path)
    if (m) out.push(m)
  }
  out.sort((a, b) => b.score - a.score || a.path.localeCompare(b.path))
  return out.slice(0, limit)
}
