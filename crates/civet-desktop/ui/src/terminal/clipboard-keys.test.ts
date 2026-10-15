import { describe, expect, it, vi } from 'vitest'
import { handleClipboardKey, isMacPlatform, type ClipboardKeyEvent } from './clipboard-keys'

function key(k: string, mods: Partial<ClipboardKeyEvent> = {}, type = 'keydown') {
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

function term(selection = '') {
  return {
    hasSelection: vi.fn(() => selection.length > 0),
    getSelection: vi.fn(() => selection),
    selectAll: vi.fn()
  }
}

describe('handleClipboardKey', () => {
  it('Cmd+C copies the selection and is consumed', () => {
    const t = term('hello')
    const copy = vi.fn()
    const ev = key('c', { metaKey: true })
    expect(handleClipboardKey(ev, t, { isMac: true, copy })).toBe(false)
    expect(copy).toHaveBeenCalledWith('hello')
    expect(ev.preventDefault).toHaveBeenCalled()
  })

  it('Cmd+C without a selection does nothing (no ^C to the pty)', () => {
    const copy = vi.fn()
    const ev = key('c', { metaKey: true })
    expect(handleClipboardKey(ev, term(''), { isMac: true, copy })).toBe(false)
    expect(copy).not.toHaveBeenCalled()
  })

  it('only copies on keydown', () => {
    const copy = vi.fn()
    expect(handleClipboardKey(key('c', { metaKey: true }, 'keyup'), term('x'), { isMac: true, copy })).toBe(false)
    expect(copy).not.toHaveBeenCalled()
  })

  it('Ctrl+C and plain keys go to xterm', () => {
    const copy = vi.fn()
    const t = term('sel')
    expect(handleClipboardKey(key('c', { ctrlKey: true }), t, { isMac: true, copy })).toBe(true)
    expect(handleClipboardKey(key('c'), t, { isMac: true, copy })).toBe(true)
    expect(copy).not.toHaveBeenCalled()
  })

  it('Cmd+V is left to the native paste event', () => {
    const ev = key('v', { metaKey: true })
    expect(handleClipboardKey(ev, term('x'), { isMac: true, copy: vi.fn() })).toBe(true)
    expect(ev.preventDefault).not.toHaveBeenCalled()
  })

  it('Cmd+A selects all in the terminal', () => {
    const t = term()
    const ev = key('a', { metaKey: true })
    expect(handleClipboardKey(ev, t, { isMac: true, copy: vi.fn() })).toBe(false)
    expect(t.selectAll).toHaveBeenCalledOnce()
    expect(ev.preventDefault).toHaveBeenCalled()
  })

  it('does nothing off macOS or with extra modifiers', () => {
    const copy = vi.fn()
    expect(handleClipboardKey(key('c', { metaKey: true }), term('x'), { isMac: false, copy })).toBe(true)
    expect(handleClipboardKey(key('C', { metaKey: true, shiftKey: true }), term('x'), { isMac: true, copy })).toBe(true)
    expect(handleClipboardKey(key('c', { metaKey: true, altKey: true }), term('x'), { isMac: true, copy })).toBe(true)
    expect(copy).not.toHaveBeenCalled()
  })
})

describe('isMacPlatform', () => {
  it('detects macOS', () => {
    expect(isMacPlatform({ platform: 'MacIntel' })).toBe(true)
    expect(isMacPlatform({ platform: '', userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' })).toBe(true)
    expect(isMacPlatform({ platform: 'Linux x86_64', userAgent: 'X11' })).toBe(false)
    expect(isMacPlatform({})).toBe(false)
  })
})
