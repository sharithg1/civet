// OS file drops onto a terminal pane (Finder, screenshots): the pane under
// the pointer gets the files' paths, or the focused pane when the drop lands
// elsewhere in the window. Remote hosts get the files uploaded first, so an
// agent there can open them. While dragging, the target pane is outlined.

import { useEffect } from 'react'
import type { CivetBackend } from '@/backend'
import { pushToast, reportError, useStore, wsKey } from '@/state/store'
import { paneController } from '@/terminal/pane-registry'
import type { PaneId } from '@/types'

type Target = { hostId: string; pane: PaneId }

function paneElementAt(x: number, y: number): HTMLElement | null {
  return document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-pane-id][data-host-id]') ?? null
}

function focusedTarget(): Target | null {
  const { active, workspaces } = useStore.getState()
  if (!active) return null
  const ws = workspaces[wsKey(active.hostId, active.worktree)]
  const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
  return tab ? { hostId: active.hostId, pane: tab.focused } : null
}

async function deliver(backend: CivetBackend, target: Target, paths: string[]): Promise<void> {
  let files
  try {
    files = await backend.prepareFileDrop(target.hostId, paths)
  } catch (err) {
    reportError('Could not drop files', err)
    return
  }
  const failed = files.filter((f) => !f.path)
  if (failed.length) {
    pushToast(
      {
        kind: 'error',
        title: `Could not drop ${failed.length} file${failed.length > 1 ? 's' : ''}`,
        body: failed.map((f) => f.error ?? f.source).join('\n')
      },
      9000
    )
  }
  const ready = files.flatMap((f) => (f.path ? [f.path] : []))
  if (ready.length) paneController(target.hostId, target.pane)?.dropPaths(ready)
}

export function useFileDrop(): void {
  const backend = useStore((s) => s.backend)
  useEffect(() => {
    if (!backend) return
    let marked: HTMLElement | null = null
    const mark = (el: HTMLElement | null) => {
      if (marked === el) return
      marked?.removeAttribute('data-drop-target')
      marked = el
      el?.setAttribute('data-drop-target', '')
    }
    let unlisten: (() => void) | undefined
    let cancelled = false
    backend
      .onFileDrop((e) => {
        if (e.type === 'leave') return mark(null)
        const el = paneElementAt(e.x, e.y)
        if (e.type === 'over') return mark(el)
        mark(null)
        const target = el
          ? { hostId: el.dataset.hostId!, pane: Number(el.dataset.paneId) }
          : focusedTarget()
        if (target && e.paths.length) void deliver(backend, target, e.paths)
      })
      .then((u) => {
        if (cancelled) u()
        else unlisten = u
      })
      .catch((err: unknown) => console.warn('[civet] file drop unavailable', err))
    return () => {
      cancelled = true
      unlisten?.()
      mark(null)
    }
  }, [backend])
}
