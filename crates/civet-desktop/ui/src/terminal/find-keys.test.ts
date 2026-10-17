import { describe, expect, it, vi } from 'vitest'
import {
  findInputAction,
  findQueryError,
  findResultLabel,
  findShortcut,
  handleFindKey,
  type FindKeyEvent
} from './find-keys'

function key(k: string, mods: Partial<FindKeyEvent> = {}, type = 'keydown') {
  return {
    type,
    key: k,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    preventDefault: vi.fn(),
    ...mods
  }
}

describe('findShortcut', () => {
  it('maps Cmd+F / Cmd+G / Shift+Cmd+G on macOS', () => {
    expect(findShortcut(key('f', { metaKey: true }), true)).toBe('open')
    expect(findShortcut(key('g', { metaKey: true }), true)).toBe('next')
    expect(findShortcut(key('G', { metaKey: true, shiftKey: true }), true)).toBe('previous')
  })

  it('ignores Ctrl+F (forward-char) and other modifiers on macOS', () => {
    expect(findShortcut(key('f', { ctrlKey: true }), true)).toBeNull()
    expect(findShortcut(key('f'), true)).toBeNull()
    expect(findShortcut(key('F', { metaKey: true, shiftKey: true }), true)).toBeNull()
    expect(findShortcut(key('f', { metaKey: true, altKey: true }), true)).toBeNull()
    expect(findShortcut(key('c', { metaKey: true }), true)).toBeNull()
  })

  it('uses Ctrl+Shift+F elsewhere, leaving Ctrl+F to the shell', () => {
    expect(findShortcut(key('F', { ctrlKey: true, shiftKey: true }), false)).toBe('open')
    expect(findShortcut(key('f', { ctrlKey: true }), false)).toBeNull()
    expect(findShortcut(key('f', { metaKey: true }), false)).toBeNull()
  })
})

describe('handleFindKey', () => {
  it('Cmd+F is consumed and opens the find bar once (keydown only)', () => {
    const onFind = vi.fn()
    const down = key('f', { metaKey: true })
    expect(handleFindKey(down, { isMac: true, onFind })).toBe(false)
    expect(down.preventDefault).toHaveBeenCalled()
    const up = key('f', { metaKey: true }, 'keyup')
    expect(handleFindKey(up, { isMac: true, onFind })).toBe(false)
    expect(onFind).toHaveBeenCalledTimes(1)
    expect(onFind).toHaveBeenCalledWith('open')
  })

  it('passes other keys through to xterm', () => {
    const onFind = vi.fn()
    const ev = key('f', { ctrlKey: true })
    expect(handleFindKey(ev, { isMac: true, onFind })).toBe(true)
    expect(ev.preventDefault).not.toHaveBeenCalled()
    expect(onFind).not.toHaveBeenCalled()
  })
})

describe('findInputAction', () => {
  it('Enter / Shift+Enter step, Esc closes', () => {
    expect(findInputAction(key('Enter'), true)).toBe('next')
    expect(findInputAction(key('Enter', { shiftKey: true }), true)).toBe('previous')
    expect(findInputAction(key('Escape'), true)).toBe('close')
  })

  it('handles the pane shortcuts too', () => {
    expect(findInputAction(key('g', { metaKey: true }), true)).toBe('next')
    expect(findInputAction(key('f', { metaKey: true }), true)).toBe('open')
  })

  it('ignores keys during IME composition and plain typing', () => {
    expect(findInputAction(key('Enter', { isComposing: true }), true)).toBeNull()
    expect(findInputAction(key('a'), true)).toBeNull()
    expect(findInputAction(key('Enter', { metaKey: true }), true)).toBeNull()
  })
})

describe('findQueryError', () => {
  it('reports invalid regexes only in regex mode', () => {
    expect(findQueryError('a(', { regex: true })).toMatch(/./)
    expect(findQueryError('a(', { regex: false })).toBeNull()
    expect(findQueryError('a+b', { regex: true })).toBeNull()
    expect(findQueryError('', { regex: true })).toBeNull()
  })
})

describe('findResultLabel', () => {
  it('formats the counter', () => {
    expect(findResultLabel('', { resultIndex: 0, resultCount: 3 }, 1000)).toBe('')
    expect(findResultLabel('x', null, 1000)).toBe('')
    expect(findResultLabel('x', { resultIndex: -1, resultCount: 0 }, 1000)).toBe('No results')
    expect(findResultLabel('x', { resultIndex: 2, resultCount: 12 }, 1000)).toBe('3 of 12')
    expect(findResultLabel('x', { resultIndex: -1, resultCount: 1 }, 1000)).toBe('1 match')
    expect(findResultLabel('x', { resultIndex: -1, resultCount: 1000 }, 1000)).toBe('1000+ matches')
    expect(findResultLabel('x', { resultIndex: 4, resultCount: 1000 }, 1000)).toBe('5 of 1000+')
  })
})
