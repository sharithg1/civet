import { useEffect, useState } from 'react'
import { getBackend } from './backend'
import { ConnectScreen } from './components/ConnectScreen'
import { AddProjectDialog, ConfirmDialog } from './components/Dialogs'
import { NewWorktreeDialog } from './components/NewWorktreeDialog'
import { RemoveWorktreeDialog } from './components/RemoveWorktreeDialog'
import { RightPanel } from './components/RightPanel'
import { SettingsView } from './components/SettingsView'
import { Sidebar } from './components/Sidebar'
import { StatusBar } from './components/StatusBar'
import { TitleBar } from './components/TitleBar'
import { Toasts } from './components/Toasts'
import { Workspace } from './components/Workspace'
import { WorktreeSwitcher } from './components/WorktreeSwitcher'
import { leaves } from './layout/pane-tree'
import { useFileDrop } from './lib/file-drop'
import { usePasteAsFile } from './lib/paste-as-file'
import {
  activateTabIndex,
  closePanes,
  cycleWorktree,
  initApp,
  newAgentTab,
  newTerminalTab,
  reportError,
  selectPreviousWorktree,
  splitFocused,
  useStore,
  worktreesOf,
  wsKey,
  closeTab,
  isPaneTab
} from './state/store'
import { initPorts } from './state/ports'

export function App() {
  const [ready, setReady] = useState(false)
  const showConnect = useStore((s) => s.showConnect)
  const hasHosts = useStore((s) => s.hostOrder.length > 0)
  const newWorktree = useStore((s) => s.newWorktree)
  const sidebarOpen = useStore((s) => s.sidebarOpen)
  const rightPanelOpen = useStore((s) => s.rightPanelOpen)

  useEffect(() => {
    let cleanup: (() => void) | undefined
    let cancelled = false
    getBackend()
      .then(async (b) => {
        const app = await initApp(b)
        const ports = await initPorts(b)
        return () => {
          ports()
          app()
        }
      })
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
  useFileDrop()
  usePasteAsFile()

  if (!ready) return null
  if (showConnect && !hasHosts) {
    return (
      <div className="relative h-full w-full">
        <ConnectScreen />
        {/* Overlay title bar: keep the window draggable above the card. */}
        <div data-tauri-drag-region className="absolute inset-x-0 top-0 h-10" />
      </div>
    )
  }
  return (
    // Cursor-style columns: the sidebar and the right panel run the full
    // window height with their own header rows; the main column stacks its
    // header (tabs), the workspace and a footer line.
    <div className="relative flex h-full w-full overflow-hidden bg-background">
      {sidebarOpen && <Sidebar />}
      <div className="flex min-w-0 flex-1 flex-col">
        <TitleBar />
        <Workspace />
        <StatusBar />
      </div>
      {rightPanelOpen && <RightPanel />}
      <SettingsView />
      <WorktreeSwitcher />
      {showConnect && (
        <ConnectScreen
          onCancel={() => useStore.setState({ showConnect: false, addProjectAfterConnect: false })}
        />
      )}
      {newWorktree && <NewWorktreeDialog key={`${newWorktree.hostId}:${newWorktree.repo ?? ''}`} />}
      <RemoveWorktreeDialog />
      <AddProjectDialog />
      <ConfirmDialog />
      <Toasts />
    </div>
  )
}

const isMac = () => navigator.platform.toLowerCase().includes('mac')

/**
 * ⌥-shortcuts must not eat characters a keyboard layout composes with Option
 * (German ⌥5 = "[", ⌥L = "@"…), which terminals need. On US-style layouts
 * ⌥ produces non-ASCII symbols or dead keys, so only those are taken.
 * Elsewhere Alt does not compose, so Alt+key is always a shortcut.
 */
function isOptionShortcut(e: KeyboardEvent): boolean {
  if (!e.altKey || e.metaKey || e.ctrlKey) return false
  if (!isMac() || e.key === 'Dead' || e.key.startsWith('Arrow')) return true
  return !(e.key.length === 1 && e.key >= ' ' && e.key <= '~')
}

/**
 * Cmd/Ctrl+T new terminal, +D split right, +Shift+D split down, +W close
 * focused pane, +K / +P switcher, +, settings; ⌥` previous worktree, ⌥↑↓
 * next/previous worktree, ⌥1-9 tab, ⌥C Claude tab, ⌥X Codex tab, ⌥T terminal
 * tab, ⌥N new worktree, ⌥B sidebar, ⌥E right panel.
 */
function useShortcuts() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const state = useStore.getState()
      const mod = isMac() ? e.metaKey : e.ctrlKey && e.shiftKey
      const overlay = !!document.querySelector('[role=dialog],[role=alertdialog]')
      if (mod && (e.code === 'KeyK' || e.code === 'KeyP')) {
        if (!state.hostOrder.length) return
        e.preventDefault()
        useStore.setState({ switcherOpen: !state.switcherOpen, settings: null })
        return
      }
      if (mod && e.code === 'KeyO' && !e.shiftKey && !overlay) {
        e.preventDefault()
        useStore.setState({ addProject: { hostId: null }, settings: null })
        return
      }
      if (mod && e.key === ',') {
        e.preventDefault()
        useStore.setState({ settings: state.settings ? null : 'general' })
        return
      }
      if (overlay || state.settings) return
      const { active, workspaces } = state
      if (mod) {
        // Tab actions are off while the worktree is being removed.
        if (!active || state.removing[wsKey(active.hostId, active.worktree)]) return
        const key = e.key.toLowerCase()
        if (key === 't') {
          e.preventDefault()
          void newTerminalTab(active.hostId, active.worktree)
        } else if (key === 'd') {
          e.preventDefault()
          void splitFocused(e.shiftKey && isMac() ? 'column' : 'row')
        } else if (key === 'w') {
          const ws = workspaces[wsKey(active.hostId, active.worktree)]
          const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
          if (!tab) return
          e.preventDefault()
          if (!isPaneTab(tab)) closeTab(wsKey(active.hostId, active.worktree), tab.id)
          else
            closePanes(active.hostId, leaves(tab.root).includes(tab.focused) ? [tab.focused] : [])
        }
        return
      }
      if (!isOptionShortcut(e)) return
      const c = e.code
      const connected = !!active && state.hosts[active.hostId]?.state === 'connected'
      let action: (() => void) | null = null
      if (c === 'Backquote') action = selectPreviousWorktree
      else if (c === 'ArrowDown' || c === 'ArrowUp')
        action = () => cycleWorktree(c === 'ArrowDown' ? 1 : -1)
      else if (c === 'KeyB') action = () => useStore.setState({ sidebarOpen: !state.sidebarOpen })
      else if (c === 'KeyE')
        action = () => useStore.setState({ rightPanelOpen: !state.rightPanelOpen })
      else if (active && /^Digit[1-9]$/.test(c))
        action = () => activateTabIndex(Number(c.slice(5)) - 1)
      else if (active && connected && c === 'KeyN') {
        const repo = worktreesOf(active.hostId).find((w) => w.id === active.worktree)?.repo
        action = () => useStore.setState({ newWorktree: { hostId: active.hostId, repo } })
      } else if (active && connected && c === 'KeyT')
        action = () => void newTerminalTab(active.hostId, active.worktree)
      else if (active && connected && c === 'KeyC')
        action = () => void newAgentTab(active.hostId, active.worktree)
      else if (active && connected && c === 'KeyX')
        action = () => void newAgentTab(active.hostId, active.worktree, 'codex')
      if (!action) return
      e.preventDefault()
      e.stopPropagation()
      action()
    }
    // Capture phase: xterm's textarea would otherwise swallow the keystroke.
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [])
}
