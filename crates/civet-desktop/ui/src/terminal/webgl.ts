// Ported from Orca: src/renderer/src/lib/pane-manager/pane-webgl-renderer.ts
// (attachWebgl / disposeWebgl / releaseXtermWebglContext) and
// terminal-webgl-addon-loader.ts. Orca's auto-policy, per-pane recovery
// latches and diagnostics were dropped: Civet tries WebGL once per pane and
// falls back to xterm's DOM renderer if the context cannot be created or is
// lost later.
import type { Terminal } from '@xterm/xterm'
import { WebglAddon } from '@xterm/addon-webgl'

type ReleasableWebglContext = {
  getExtension(name: 'WEBGL_lose_context'): WEBGL_lose_context | null
}
type XtermWebglAddonInternals = {
  _renderer?: { _gl?: ReleasableWebglContext; _canvas?: HTMLCanvasElement }
}

export type WebglHandle = {
  readonly active: boolean
  dispose(): void
}

/** Process-wide kill switch once WebGL proved unusable (e.g. WebKitGTK without GPU). */
let webglBroken = false

export function attachWebgl(term: Terminal, onFallback?: (reason: string) => void): WebglHandle {
  let addon: WebglAddon | null = null
  const handle = {
    get active() {
      return addon !== null
    },
    dispose() {
      if (!addon) return
      releaseXtermWebglContext(addon)
      try {
        addon.dispose()
      } catch {
        /* ignore — teardown must not block the DOM fallback */
      }
      addon = null
    }
  }
  if (webglBroken || !supportsWebgl2()) {
    onFallback?.('WebGL2 unavailable')
    return handle
  }
  try {
    const a = new WebglAddon()
    a.onContextLoss(() => {
      // Why (Orca): a lost context leaves a blank canvas; dispose so xterm
      // repaints with the DOM renderer instead of freezing.
      handle.dispose()
      try {
        term.refresh(0, term.rows - 1)
      } catch {
        /* pane may already be disposed */
      }
      onFallback?.('WebGL context lost')
    })
    term.loadAddon(a)
    addon = a
  } catch (err) {
    webglBroken = true
    addon = null
    onFallback?.(`WebGL attach failed: ${err instanceof Error ? err.message : String(err)}`)
  }
  return handle
}

function supportsWebgl2(): boolean {
  try {
    const canvas = document.createElement('canvas')
    const gl = canvas.getContext('webgl2')
    gl?.getExtension('WEBGL_lose_context')?.loseContext()
    return gl !== null
  } catch {
    return false
  }
}

function releaseXtermWebglContext(addon: WebglAddon): void {
  try {
    // Why (Orca #6874): xterm removes the canvas on dispose, but some drivers
    // keep the context alive long enough to hit the browser's WebGL context
    // budget when many panes open and close quickly.
    const renderer = (addon as unknown as XtermWebglAddonInternals)._renderer
    renderer?._gl?.getExtension('WEBGL_lose_context')?.loseContext()
    if (renderer?._canvas) {
      renderer._canvas.width = 0
      renderer._canvas.height = 0
    }
  } catch {
    /* ignore */
  }
}
