import { describe, expect, it } from 'vitest'
import { clampSidebarWidth, DEFAULT_PREFS, resetPrefs, setPrefs, usePrefs } from './prefs'

describe('prefs', () => {
  it('clamps the sidebar width to the drag limits', () => {
    expect(clampSidebarWidth(100)).toBe(200)
    expect(clampSidebarWidth(320.6)).toBe(321)
    expect(clampSidebarWidth(9000)).toBe(480)
    expect(clampSidebarWidth(NaN)).toBe(DEFAULT_PREFS.sidebarWidth)
  })

  it('keeps the sidebar width on "Reset to defaults" (it is layout, not a setting)', () => {
    setPrefs({ sidebarWidth: 340, codexCommand: 'codex --full-auto' })
    resetPrefs()
    expect(usePrefs.getState().sidebarWidth).toBe(340)
    expect(usePrefs.getState().codexCommand).toBe('codex')
  })
})
