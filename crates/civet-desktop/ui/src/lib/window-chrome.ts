// macOS overlay title bar (tauri.conf.json: titleBarStyle "Overlay",
// hiddenTitle, trafficLightPosition): the web content starts at the very top
// of the window, so the v3 title bar doubles as the tab row. It needs
//  * a left inset so the traffic lights (x 14, y 13) do not cover the wordmark
//    (none in fullscreen, where macOS hides them), and
//  * `data-tauri-drag-region` on its empty parts (window drag; a double click
//    zooms, via Tauri's drag script + `core:window:allow-start-dragging`).
// Outside Tauri (mock UI in a browser) there is no overlay unless `?overlay`
// is in the URL (to preview the layout).

import { useEffect, useState } from 'react'
import { isTauri } from '@tauri-apps/api/core'

/** Width reserved for the three traffic lights (14 + 3×14 + 2×8 + gap). */
export const TRAFFIC_LIGHTS_INSET = 78

function isMac(): boolean {
  return typeof navigator !== 'undefined' && /mac/i.test(navigator.platform)
}

/** True when the window draws its content under a transparent macOS title bar. */
export function hasOverlayTitleBar(): boolean {
  if (typeof location !== 'undefined' && new URLSearchParams(location.search).has('overlay')) return true
  return isMac() && isTauri()
}

/** Left padding for the title bar row: the traffic lights' width, 0 in fullscreen. */
export function useTitleBarInset(): number {
  const overlay = hasOverlayTitleBar()
  const [fullscreen, setFullscreen] = useState(false)
  useEffect(() => {
    if (!overlay || !isTauri()) return
    let stop: (() => void) | undefined
    let cancelled = false
    void import('@tauri-apps/api/window')
      .then(async ({ getCurrentWindow }) => {
        const w = getCurrentWindow()
        const check = () => void w.isFullscreen().then((f) => !cancelled && setFullscreen(f)).catch(() => {})
        check()
        const un = await w.onResized(check)
        if (cancelled) un()
        else stop = un
      })
      .catch(() => {})
    return () => {
      cancelled = true
      stop?.()
    }
  }, [overlay])
  return overlay && !fullscreen ? TRAFFIC_LIGHTS_INSET : 0
}
