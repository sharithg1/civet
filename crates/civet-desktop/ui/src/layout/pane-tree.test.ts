import { describe, expect, it } from 'vitest'
import { leaf, leaves, removeLeaf, setRatio, siblingLeaf, splitLeaf } from './pane-tree'

describe('pane tree', () => {
  it('splits, lists and removes leaves', () => {
    let t = leaf(1)
    t = splitLeaf(t, 1, 2, 'row')
    t = splitLeaf(t, 2, 3, 'column')
    expect(leaves(t)).toEqual([1, 2, 3])
    expect(siblingLeaf(t, 2)).toBe(3)
    const r = removeLeaf(t, 2)!
    expect(leaves(r)).toEqual([1, 3])
    expect(r.type === 'split' && r.dir).toBe('row')
    expect(removeLeaf(leaf(1), 1)).toBeNull()
    expect(removeLeaf(t, 99)).toBe(t)
  })

  it('clamps split ratios along a path', () => {
    const t = splitLeaf(splitLeaf(leaf(1), 1, 2, 'row'), 2, 3, 'column')
    const u = setRatio(t, ['b'], 0.99)
    expect(u.type === 'split' && u.b.type === 'split' && u.b.ratio).toBe(0.9)
    const v = setRatio(t, [], 0.01)
    expect(v.type === 'split' && v.ratio).toBe(0.1)
  })
})
