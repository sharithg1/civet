import { describe, expect, it } from 'vitest'
import { diffSides, parseUnifiedDiff, splitPath } from './diff'

const num = (d: ReturnType<typeof parseUnifiedDiff>, h = 0) =>
  d.hunks[h]!.lines.map((l) => [l.kind, l.oldNo, l.newNo, l.text])

describe('parseUnifiedDiff', () => {
  it('numbers context, added and deleted lines', () => {
    const d = parseUnifiedDiff(
      [
        'diff --git a/x.rs b/x.rs',
        'index 1..2 100644',
        '--- a/x.rs',
        '+++ b/x.rs',
        '@@ -10,4 +10,4 @@ fn main() {',
        ' a',
        '-b',
        '+B',
        '+C',
        ' c',
        '@@ -40 +40,0 @@',
        '-gone',
        '\\ No newline at end of file',
        ''
      ].join('\n')
    )
    expect(d.combined).toBe(false)
    expect(d.headers).toHaveLength(4)
    expect(d.hunks.map((h) => h.header)).toEqual([
      '@@ -10,4 +10,4 @@ fn main() {',
      '@@ -40 +40,0 @@'
    ])
    expect(num(d)).toEqual([
      ['context', 10, 10, 'a'],
      ['del', 11, null, 'b'],
      ['add', null, 11, 'B'],
      ['add', null, 12, 'C'],
      ['context', 12, 13, 'c']
    ])
    expect(num(d, 1)).toEqual([
      ['del', 40, null, 'gone'],
      ['note', null, null, 'No newline at end of file']
    ])
    expect(d.lineCount).toBe(7)
  })

  it('handles a new file and an empty diff', () => {
    const d = parseUnifiedDiff('--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+x\n+\n')
    expect(num(d)).toEqual([
      ['add', null, 1, 'x'],
      ['add', null, 2, '']
    ])
    expect(parseUnifiedDiff('')).toEqual({ headers: [], hunks: [], combined: false, lineCount: 0 })
  })

  it('reads a combined diff of a conflict', () => {
    const d = parseUnifiedDiff(
      [
        'diff --cc a.txt',
        '@@@ -1,1 -1,1 +1,5 @@@',
        '++<<<<<<< HEAD',
        ' +main',
        '++=======',
        '+ other',
        '++>>>>>>> other',
        '  same',
        ''
      ].join('\n')
    )
    expect(d.combined).toBe(true)
    expect(num(d)).toEqual([
      ['add', null, 1, '<<<<<<< HEAD'],
      ['add', null, 2, 'main'],
      ['add', null, 3, '======='],
      ['add', null, 4, 'other'],
      ['add', null, 5, '>>>>>>> other'],
      ['context', null, 6, 'same']
    ])
  })
})

describe('splitPath', () => {
  it('splits name and directory', () => {
    expect(splitPath('src/a/b.rs')).toEqual(['b.rs', 'src/a'])
    expect(splitPath('README')).toEqual(['README', ''])
  })
})

describe('diffSides', () => {
  it('splits a diff into old and new code, mapping each line to its row', () => {
    const d = parseUnifiedDiff(
      [
        '@@ -1,3 +1,3 @@',
        ' a',
        '-b',
        '+B',
        ' c',
        '@@ -9 +9,0 @@',
        '-z',
        '\\ No newline at end of file'
      ].join('\n')
    )
    const s = diffSides(d)
    expect(s.old).toBe('a\nb\nc\nz')
    expect(s.new).toBe('a\nB\nc')
    expect(s.rows).toEqual([
      [
        { side: 'new', row: 0 },
        { side: 'old', row: 1 },
        { side: 'new', row: 1 },
        { side: 'new', row: 2 }
      ],
      [{ side: 'old', row: 3 }, null]
    ])
  })
})
