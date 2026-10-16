import { describe, expect, it } from 'vitest'
import { handleShiftEnter, SHIFT_ENTER_SEQUENCE, type NewlineKeyEvent } from './newline-keys'

const key = (p: Partial<NewlineKeyEvent>): NewlineKeyEvent => ({
  type: 'keydown',
  key: 'Enter',
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  preventDefault: () => {},
  ...p
})

describe('Shift+Enter', () => {
  it('sends ESC CR once and swallows the key', () => {
    const sent: string[] = []
    expect(handleShiftEnter(key({ shiftKey: true }), (d) => sent.push(d))).toBe(false)
    expect(handleShiftEnter(key({ shiftKey: true, type: 'keypress' }), (d) => sent.push(d))).toBe(
      false
    )
    expect(handleShiftEnter(key({ shiftKey: true, type: 'keyup' }), (d) => sent.push(d))).toBe(
      false
    )
    expect(sent).toEqual([SHIFT_ENTER_SEQUENCE])
  })

  it('leaves Enter and other combinations to xterm', () => {
    const send = () => {
      throw new Error('sent')
    }
    expect(handleShiftEnter(key({}), send)).toBe(true)
    expect(handleShiftEnter(key({ shiftKey: true, metaKey: true }), send)).toBe(true)
    expect(handleShiftEnter(key({ shiftKey: true, key: 'a' }), send)).toBe(true)
  })
})
