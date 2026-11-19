import { describe, expect, it } from 'vitest'
import {
  MAX_CANDIDATES_PER_LINE,
  findPathCandidates,
  findUrls,
  formatFileSize,
  relativeTo
} from './terminal-paths'

const paths = (text: string) => findPathCandidates(text).map((c) => c.path)

describe('findPathCandidates', () => {
  it('finds absolute, home and relative paths', () => {
    expect(paths('Saved the screenshot to /tmp/shots/home.png')).toEqual(['/tmp/shots/home.png'])
    expect(paths('see ~/notes/plan.md and ./out/report.pdf')).toEqual([
      '~/notes/plan.md',
      './out/report.pdf'
    ])
    expect(paths('⏺ Update(src/components/App.tsx)')).toEqual(['src/components/App.tsx'])
    expect(paths('wrote ../sibling/a_b-c.txt')).toEqual(['../sibling/a_b-c.txt'])
    expect(paths('│ README.md │ package.json │')).toEqual(['README.md', 'package.json'])
    expect(paths('"/var/log/app.log", `dist/a.js`')).toEqual(['/var/log/app.log', 'dist/a.js'])
  })

  it('reports where the candidate is, with its line suffix', () => {
    const text = 'error at src/app.ts:12:5, then lib/x.rs:7.'
    const [a, b] = findPathCandidates(text)
    expect(a).toEqual({ start: 9, end: 24, path: 'src/app.ts', line: 12 })
    expect(text.slice(a!.start, a!.end)).toBe('src/app.ts:12:5')
    expect(b).toMatchObject({ path: 'lib/x.rs', line: 7 })
    expect(text.slice(b!.start, b!.end)).toBe('lib/x.rs:7')
  })

  it('drops the end of a sentence', () => {
    const text = 'It is in docs/guide.md.'
    const [c] = findPathCandidates(text)
    expect(text.slice(c!.start, c!.end)).toBe('docs/guide.md')
    expect(paths('See /tmp/out...')).toEqual(['/tmp/out'])
  })

  it('leaves out what is not a file path', () => {
    expect(paths('visit https://example.com/docs/a.html now')).toEqual([])
    expect(paths('scp host:/srv/a.txt .')).toEqual([])
    expect(paths('civetd 0.6.1 and v1.2')).toEqual([])
    expect(paths('mail me@example.com')).toEqual([])
    expect(paths('cd / && cd .. && cd ~/ && ls ./')).toEqual([])
    expect(paths('plain words, no paths here')).toEqual([])
    expect(paths('git@github.com:owner/repo.git')).toEqual([])
  })

  it('keeps prose that only looks like a path as a candidate (the host decides)', () => {
    expect(paths('read and/or write')).toEqual(['and/or'])
  })

  it('caps the candidates of one line', () => {
    const text = Array.from({ length: 20 }, (_, i) => `d/f${i}.txt`).join(' ')
    expect(findPathCandidates(text)).toHaveLength(MAX_CANDIDATES_PER_LINE)
  })
})

describe('relativeTo', () => {
  it('is the path inside the directory, or null', () => {
    expect(relativeTo('/w/repo', '/w/repo/src/a.ts')).toBe('src/a.ts')
    expect(relativeTo('/w/repo/', '/w/repo/a')).toBe('a')
    expect(relativeTo('/w/repo', '/w/repo-2/a')).toBeNull()
    expect(relativeTo('/w/repo', '/w/repo')).toBeNull()
    expect(relativeTo('/w/repo', '/tmp/a')).toBeNull()
  })
})

describe('formatFileSize', () => {
  it('uses the largest unit that fits', () => {
    expect(formatFileSize(0)).toBe('0 B')
    expect(formatFileSize(999)).toBe('999 B')
    expect(formatFileSize(1500)).toBe('1.5 KB')
    expect(formatFileSize(340_000)).toBe('340 KB')
    expect(formatFileSize(2_340_000)).toBe('2.3 MB')
  })
})

describe('findUrls', () => {
  const urls = (text: string) => findUrls(text).map((u) => u.url)

  it('finds web URLs and where they are', () => {
    const text = 'Local: http://localhost:5173/ and docs at https://example.com/a?b=1#c'
    const found = findUrls(text)
    expect(found.map((u) => u.url)).toEqual([
      'http://localhost:5173/',
      'https://example.com/a?b=1#c'
    ])
    expect(text.slice(found[1]!.start, found[1]!.end)).toBe('https://example.com/a?b=1#c')
  })

  it('leaves the surrounding punctuation out', () => {
    expect(urls('See https://example.com/docs.')).toEqual(['https://example.com/docs'])
    expect(urls('(PR: https://github.com/o/r/pull/48), merged')).toEqual([
      'https://github.com/o/r/pull/48'
    ])
    expect(urls('https://en.wikipedia.org/wiki/Rust_(language)')).toEqual([
      'https://en.wikipedia.org/wiki/Rust_(language)'
    ])
    expect(urls('"https://a.dev/x" <https://b.dev/y> │https://c.dev/z│')).toEqual([
      'https://a.dev/x',
      'https://b.dev/y',
      'https://c.dev/z'
    ])
  })

  it('ignores other schemes and bare schemes', () => {
    expect(urls('file:///etc/passwd ftp://x.y/z ssh://h javascript:alert(1)')).toEqual([])
    expect(urls('use https:// or http://.')).toEqual([])
  })
})
