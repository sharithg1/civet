import { describe, expect, it } from 'vitest'
import { applyOrder } from '../state/prefs'
import { moveTo } from './reorder'

describe('sidebar ordering', () => {
  it('moveTo: insertion index before removal', () => {
    expect(moveTo(['a', 'b', 'c'], 'a', 3)).toEqual(['b', 'c', 'a'])
    expect(moveTo(['a', 'b', 'c'], 'c', 0)).toEqual(['c', 'a', 'b'])
    expect(moveTo(['a', 'b', 'c'], 'a', 2)).toEqual(['b', 'a', 'c'])
    expect(moveTo(['a', 'b', 'c'], 'b', 1)).toEqual(['a', 'b', 'c'])
  })

  it('applyOrder: saved order first, unknown ids after, stale ids dropped', () => {
    expect(applyOrder(['a', 'b', 'c'], ['c', 'gone', 'a'])).toEqual(['c', 'a', 'b'])
    expect(applyOrder(['a', 'b'], undefined)).toEqual(['a', 'b'])
  })
})
