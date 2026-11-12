import { describe, expect, it } from 'vitest'
import {
  handleWordKey,
  WORD_LEFT_SEQUENCE,
  WORD_RIGHT_SEQUENCE,
  type WordKeyEvent
} from './word-keys'

const key = (p: Partial<WordKeyEvent>): WordKeyEvent => ({
  type: 'keydown',
  key: 'ArrowLeft',
  metaKey: false,
  ctrlKey: false,
  altKey: true,
  shiftKey: false,
  preventDefault: () => {},
  ...p
})

describe('Option+Left/Right', () => {
  it('sends ESC b / ESC f once and swallows the key', () => {
    const sent: string[] = []
    const send = (d: string) => sent.push(d)
    expect(handleWordKey(key({}), true, send)).toBe(false)
    expect(handleWordKey(key({ type: 'keyup' }), true, send)).toBe(false)
    expect(handleWordKey(key({ key: 'ArrowRight' }), true, send)).toBe(false)
    expect(sent).toEqual([WORD_LEFT_SEQUENCE, WORD_RIGHT_SEQUENCE])
  })

  it('leaves other platforms and combinations to xterm', () => {
    const send = () => {
      throw new Error('sent')
    }
    expect(handleWordKey(key({}), false, send)).toBe(true)
    expect(handleWordKey(key({ altKey: false }), true, send)).toBe(true)
    expect(handleWordKey(key({ shiftKey: true }), true, send)).toBe(true)
    expect(handleWordKey(key({ metaKey: true }), true, send)).toBe(true)
    expect(handleWordKey(key({ key: 'ArrowUp' }), true, send)).toBe(true)
    expect(handleWordKey(key({ key: 'b' }), true, send)).toBe(true)
  })
})
