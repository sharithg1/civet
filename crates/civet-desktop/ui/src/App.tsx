import { useEffect, useState } from 'react'
import { getBackend } from './backend'
import { ConnectScreen } from './components/ConnectScreen'
import { AddRepoDialog, ConfirmDialog } from './components/Dialogs'
import { NewWorktreeDialog } from './components/NewWorktreeDialog'
import { Sidebar } from './components/Sidebar'
import { Toasts } from './components/Toasts'
import { Workspace } from './components/Workspace'
import { leaves } from './layout/pane-tree'
import {
  closePanes,
  initApp,
  newTerminalTab,
  reportError,
  splitFocused,
  useStore,
  wsKey
} from './state/store'

export function App() {
  const [ready, setReady] = useState(false)
  const showConnect = useStore((s) => s.showConnect)
  const hasHosts = useStore((s) => s.hostOrder.length > 0)
  const newWorktree = useStore((s) => s.newWorktree)

  useEffect(() => {
    let cleanup: (() => void) | undefined
    let cancelled = false
    getBackend()
      .then((b) => initApp(b))
      .then((c) => {
        if (cancelled) c()
        else cleanup = c
        setReady(true)
      })
      .catch((err) => {
        reportError('Could not start', err)
        setReady(true)
      })
    return () => {
      cancelled = true
      cleanup?.()
    }
  }, [])

  useShortcuts()

  if (!ready) return null
  if (showConnect && !hasHosts) return <ConnectScreen />
  return (
    <div className="flex h-full w-full overflow-hidden">
      <Sidebar />
      <Workspace />
      {showConnect && (
        <div className="fixed inset-0 z-50 bg-black/55 backdrop-blur-[2px]">
          <ConnectScreen onCancel={() => useStore.setState({ showConnect: false })} />
        </div>
      )}
      {newWorktree && <NewWorktreeDialog key={`${newWorktree.hostId}:${newWorktree.repo ?? ''}`} />}
      <AddRepoDialog />
      <ConfirmDialog />
      <Toasts />
    </div>
  )
}

/** Cmd/Ctrl+T new terminal, +D split right, +Shift+D split down, +W close focused pane. */
function useShortcuts() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = navigator.platform.toLowerCase().includes('mac') ? e.metaKey : e.ctrlKey && e.shiftKey
      if (!mod) return
      const { active, workspaces } = useStore.getState()
      if (!active) return
      const key = e.key.toLowerCase()
      if (key === 't') {
        e.preventDefault()
        void newTerminalTab(active.hostId, active.worktree)
      } else if (key === 'd') {
        e.preventDefault()
        void splitFocused(e.shiftKey && navigator.platform.toLowerCase().includes('mac') ? 'column' : 'row')
      } else if (key === 'w') {
        const ws = workspaces[wsKey(active.hostId, active.worktree)]
        const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
        if (!tab) return
        e.preventDefault()
        closePanes(active.hostId, leaves(tab.root).includes(tab.focused) ? [tab.focused] : [])
      }
    }
    // Capture phase: xterm's textarea would otherwise swallow the keystroke.
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [])
}
