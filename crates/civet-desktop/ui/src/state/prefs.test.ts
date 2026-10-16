import { describe, expect, it } from 'vitest'
import {
  clampRightPanelWidth,
  clampSidebarWidth,
  DEFAULT_PREFS,
  resetPrefs,
  setPrefs,
  usePrefs
} from './prefs'

describe('prefs', () => {
  it('clamps the sidebar width to the drag limits', () => {
    expect(clampSidebarWidth(100)).toBe(200)
    expect(clampSidebarWidth(320.6)).toBe(321)
    expect(clampSidebarWidth(9000)).toBe(480)
    expect(clampSidebarWidth(NaN)).toBe(DEFAULT_PREFS.sidebarWidth)
  })

  it('clamps the right panel width to the drag limits', () => {
    expect(clampRightPanelWidth(100)).toBe(280)
    expect(clampRightPanelWidth(420.4)).toBe(420)
    expect(clampRightPanelWidth(9000)).toBe(800)
    expect(clampRightPanelWidth(NaN)).toBe(DEFAULT_PREFS.rightPanelWidth)
  })

  it('keeps the panel widths on "Reset to defaults" (they are layout, not settings)', () => {
    setPrefs({ sidebarWidth: 340, rightPanelWidth: 500, codexCommand: 'codex --full-auto' })
    resetPrefs()
    expect(usePrefs.getState().sidebarWidth).toBe(340)
    expect(usePrefs.getState().rightPanelWidth).toBe(500)
    expect(usePrefs.getState().codexCommand).toBe('codex')
  })
})
