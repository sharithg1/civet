// Mounted pane controllers by (host, pane), for app-level input that targets
// a pane from outside its React subtree (OS file drops).
import type { PaneId } from '../types'
import type { PaneController } from './pane-controller'

const controllers = new Map<string, PaneController>()
const k = (hostId: string, pane: PaneId) => `${hostId}\u0000${pane}`

/** Register a mounted controller; returns the unregister function. */
export function registerPane(c: PaneController): () => void {
  const key = k(c.hostId, c.pane)
  controllers.set(key, c)
  return () => {
    if (controllers.get(key) === c) controllers.delete(key)
  }
}

export function paneController(hostId: string, pane: PaneId): PaneController | undefined {
  return controllers.get(k(hostId, pane))
}
