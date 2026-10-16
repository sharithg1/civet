import { describe, expect, it } from 'vitest'
import { Slugger, isDataImage, isMarkdownPath, resolveLink, slugify } from './markdown'

describe('isMarkdownPath', () => {
  it('matches markdown extensions only', () => {
    expect(isMarkdownPath('README.md')).toBe(true)
    expect(isMarkdownPath('docs/a.MARKDOWN')).toBe(true)
    expect(isMarkdownPath('src/md.ts')).toBe(false)
    expect(isMarkdownPath('md')).toBe(false)
  })
})

describe('resolveLink', () => {
  it('opens web links in the browser', () => {
    expect(resolveLink('a.md', 'https://x.dev/a')).toEqual({ kind: 'web', url: 'https://x.dev/a' })
  })

  it('refuses other schemes', () => {
    expect(resolveLink('a.md', 'javascript:alert(1)')).toBeNull()
    expect(resolveLink('a.md', 'file:///etc/passwd')).toBeNull()
    expect(resolveLink('a.md', '//evil.com/x')).toBeNull()
    expect(resolveLink('a.md', 'mailto:a@b.c')).toBeNull()
    // The desktop's open_web_url only takes lowercase schemes.
    expect(resolveLink('a.md', 'HTTP://x.dev')).toBeNull()
    expect(resolveLink('a.md', '')).toBeNull()
  })

  it('resolves anchors in the same file', () => {
    expect(resolveLink('a.md', '#getting-started')).toEqual({
      kind: 'anchor',
      id: 'getting-started'
    })
    expect(resolveLink('a.md', '#')).toBeNull()
  })

  it('resolves worktree files relative to the file', () => {
    expect(resolveLink('docs/guide/a.md', 'b.md')).toEqual({
      kind: 'file',
      path: 'docs/guide/b.md',
      hash: ''
    })
    expect(resolveLink('docs/guide/a.md', './../INTERFACES.md#types')).toEqual({
      kind: 'file',
      path: 'docs/INTERFACES.md',
      hash: 'types'
    })
    expect(resolveLink('docs/a.md', '/crates/x/README.md?plain=1')).toEqual({
      kind: 'file',
      path: 'crates/x/README.md',
      hash: ''
    })
    expect(resolveLink('a.md', 'my%20notes.md')).toEqual({
      kind: 'file',
      path: 'my notes.md',
      hash: ''
    })
  })

  it('refuses paths outside the worktree', () => {
    expect(resolveLink('docs/a.md', '../../x.md')).toBeNull()
    expect(resolveLink('a.md', '.')).toBeNull()
  })
})

describe('slugs', () => {
  it('follows GitHub', () => {
    expect(slugify('Getting Started!')).toBe('getting-started')
    expect(slugify('`civet handoff` & co.')).toBe('civet-handoff--co')
    const s = new Slugger()
    expect([s.slug('Usage'), s.slug('Usage'), s.slug('Usage')]).toEqual([
      'usage',
      'usage-1',
      'usage-2'
    ])
    const t = new Slugger()
    expect([t.slug('A-1'), t.slug('A'), t.slug('A')]).toEqual(['a-1', 'a', 'a-2'])
  })
})

describe('isDataImage', () => {
  it('takes raster data URLs only', () => {
    expect(isDataImage('data:image/png;base64,iVBOR')).toBe(true)
    expect(isDataImage('data:image/jpeg,xyz')).toBe(true)
    expect(isDataImage('data:image/svg+xml,<svg/>')).toBe(false)
    expect(isDataImage('data:text/html,<b>')).toBe(false)
    expect(isDataImage('https://x.dev/a.png')).toBe(false)
  })
})
