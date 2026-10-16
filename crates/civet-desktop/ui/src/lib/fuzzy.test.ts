import { describe, expect, it } from 'vitest'
import { fuzzyFilter, fuzzyMatch } from './fuzzy'

describe('fuzzyMatch', () => {
  it('needs every character in order', () => {
    expect(fuzzyMatch('ctl', 'src/control.rs')?.positions).toEqual([4, 7, 10])
    expect(fuzzyMatch('xyz', 'src/control.rs')).toBeNull()
    expect(fuzzyMatch('lc', 'cl')).toBeNull()
  })

  it('ignores case and spaces; an empty query matches everything', () => {
    expect(fuzzyMatch('READ me', 'README.md')).not.toBeNull()
    expect(fuzzyMatch('', 'a/b')).toEqual({ path: 'a/b', score: 0, positions: [] })
  })
})

describe('fuzzyFilter', () => {
  it('ranks file-name and consecutive matches first', () => {
    const paths = ['crates/civet-proto/src/frame.rs', 'docs/frontend/README.md', 'src/main.rs']
    expect(fuzzyFilter('frame', paths).map((m) => m.path)).toEqual([
      'crates/civet-proto/src/frame.rs',
      'docs/frontend/README.md'
    ])
    expect(fuzzyFilter('main', [...paths, 'crates/domain/x.rs'])[0]!.path).toBe('src/main.rs')
  })

  it('caps the result count', () => {
    const paths = Array.from({ length: 50 }, (_, i) => `f${i}.ts`)
    expect(fuzzyFilter('f', paths, 10)).toHaveLength(10)
  })
})
