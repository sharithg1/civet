// Main area for the selected worktree: connection banner and the active
// tab's split layout (tabs live in the TitleBar). Only the active tab is
// mounted; switching tabs detaches the hidden panes and re-attaching replays
// a Full (cheap in the mosh model, and it keeps WebGL contexts bounded).
import { useStore, wsKey as makeWsKey } from '@/state/store'
import { leaves } from '@/layout/pane-tree'
import { ConnectionBanner } from './ConnectionBanner'
import { SplitLayout } from './SplitLayout'

export function Workspace() {
  const active = useStore((s) => s.active)
  const key = active ? makeWsKey(active.hostId, active.worktree) : ''
  const ws = useStore((s) => (key ? s.workspaces[key] : undefined))

  if (!active) {
    return (
      <main className="grid min-w-0 flex-1 place-items-center bg-background font-mono text-[12px] text-faint">
        select a worktree · ⌘K switch
      </main>
    )
  }
  const tab = ws?.tabs.find((t) => t.id === ws?.activeTab)
  return (
    <main className="flex min-w-0 flex-1 flex-col bg-background">
      <ConnectionBanner hostId={active.hostId} />
      <div className="relative min-h-0 flex-1">
        {tab ? (
          <SplitLayout
            key={tab.id}
            hostId={active.hostId}
            wsKey={key}
            tabId={tab.id}
            node={tab.root}
            focused={tab.focused}
            multiPane={leaves(tab.root).length > 1}
          />
        ) : (
          <div className="grid h-full place-items-center font-mono text-[12px] text-faint">
            no tabs · ⌥C claude · ⌥T terminal
          </div>
        )}
      </div>
    </main>
  )
}
