import { describe, expect, it } from 'vitest'
import { releaseDateLabel, versionInfoText } from './updates'

describe('releaseDateLabel', () => {
  const now = new Date('2026-10-16T12:00:00Z')
  it('shows the date and how long ago', () => {
    expect(releaseDateLabel('2026-10-14T18:46:00Z', now)).toBe('Oct 14, 2026 (1 day ago)')
    expect(releaseDateLabel('2026-10-10T12:00:00Z', now)).toBe('Oct 10, 2026 (6 days ago)')
    expect(releaseDateLabel('2026-10-16T09:00:00Z', now)).toMatch(/\(today\)$/)
  })
  it('is empty for an unknown date', () => {
    expect(releaseDateLabel('', now)).toBe('')
    expect(releaseDateLabel('not a date', now)).toBe('')
  })
})

describe('versionInfoText', () => {
  it('lists version, arch, commit and OS, skipping unknowns', () => {
    expect(
      versionInfoText({
        version: '0.1.12',
        arch: 'aarch64',
        commit: 'abc123',
        commitDate: '',
        os: 'macOS 15.4'
      })
    ).toBe('Civet 0.1.12 (Apple Silicon)\nCommit: abc123\nOS: macOS 15.4')
  })
})
