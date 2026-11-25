import { describe, expect, it } from 'vitest'
import { branchSlug, freeBranch } from './branch-name'

describe('branchSlug', () => {
  it('makes the slug civet handoff makes', () => {
    expect(branchSlug('Fix the flaky `ports` test!')).toBe('fix-the-flaky-ports-test')
    expect(branchSlug('Bump tokio → 1.54')).toBe('bump-tokio-1-54')
    expect(branchSlug('日本語')).toBe('')
    expect(branchSlug('  ')).toBe('')
  })

  it('keeps whole words up to 48 characters', () => {
    const long = branchSlug('word '.repeat(30))
    expect(long.length).toBeLessThanOrEqual(48)
    expect(long.endsWith('word')).toBe(true)
    expect(branchSlug('x'.repeat(80))).toHaveLength(48)
  })
})

describe('freeBranch', () => {
  it('suffixes a taken name', () => {
    expect(freeBranch('a', new Set())).toBe('a')
    expect(freeBranch('a', new Set(['a']))).toBe('a-2')
    expect(freeBranch('a', new Set(['a', 'a-2']))).toBe('a-3')
  })
})
