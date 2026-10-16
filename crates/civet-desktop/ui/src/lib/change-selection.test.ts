import { describe, expect, it } from 'vitest'
import { selectChange, selectedPaths, type ChangeSelection } from './change-selection'

const order = ['a', 'b', 'c', 'd']

describe('change selection', () => {
  it('selects one row on a plain click, toggles with ⌘ and extends with ⇧', () => {
    let sel: ChangeSelection = selectChange(null, 'unstaged', order, 'b')
    expect(sel).toEqual({ side: 'unstaged', paths: ['b'], anchor: 'b' })
    sel = selectChange(sel, 'unstaged', order, 'd', { toggle: true })
    expect(sel?.paths).toEqual(['b', 'd'])
    sel = selectChange(sel, 'unstaged', order, 'a', { range: true })
    // The range runs from the anchor (the last toggled row) to the click.
    expect(sel?.paths).toEqual(['a', 'b', 'c', 'd'])
    sel = selectChange(sel, 'unstaged', order, 'c', { toggle: true })
    expect(sel?.paths).toEqual(['a', 'b', 'd'])
    sel = selectChange(selectChange(null, 'unstaged', order, 'c'), 'unstaged', order, 'c', {
      toggle: true
    })
    expect(sel).toBeNull()
  })

  it('stays within one group and drops rows that are gone', () => {
    let sel = selectChange(null, 'unstaged', order, 'a')
    sel = selectChange(sel, 'unstaged', order, 'c', { range: true })
    // ⇧-click in another group starts over there.
    sel = selectChange(sel, 'staged', ['x', 'y'], 'y', { range: true })
    expect(sel).toEqual({ side: 'staged', paths: ['y'], anchor: 'y' })
    sel = selectChange(sel, 'staged', ['x', 'y'], 'x', { toggle: true })
    expect(selectedPaths(sel, 'staged', ['y'])).toEqual(['y'])
    expect(selectedPaths(sel, 'unstaged', order)).toEqual([])
  })
})
