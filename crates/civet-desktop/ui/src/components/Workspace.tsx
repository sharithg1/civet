// Main area for the selected worktree: connection banner, tab bar and the
// active tab's split layout. Only the active tab is mounted; switching tabs
// detaches the hidden panes and re-attaching replays a Full (cheap in the
// mosh model, and it keeps WebGL contexts bounded).
import { Plus, Sparkles } from 'lucide-react'
import { newTerminalTab, useStore, wsKey as makeWsKey } from '../state/store'
import { leaves } from '../layout/pane-tree'
import { basename } from '../lib/utils'
import { ConnectionBanner } from './ConnectionBanner'
import { SplitLayout } from './SplitLayout'
import { TabBar } from './TabBar'
import { Button } from './ui'

export function Workspace() {
  const active = useStore((s) => s.active)
  const key = active ? makeWsKey(active.hostId, active.worktree) : ''
  const ws = useStore((s) => (key ? s.workspaces[key] : undefined))
  const hostState = useStore((s) => (active ? s.hosts[active.hostId]?.state : undefined))

  if (!active) {
    return (
      <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
        Select a worktree in the sidebar.
      </div>
    )
  }
  const tabs = ws?.tabs ?? []
  const tab = tabs.find((t) => t.id === ws?.activeTab)
  return (
    <main className="flex min-w-0 flex-1 flex-col bg-background">
      <ConnectionBanner hostId={active.hostId} />
      <TabBar
        hostId={active.hostId}
        worktree={active.worktree}
        wsKey={key}
        tabs={tabs}
        activeTab={ws?.activeTab ?? null}
      />
      <div className="relative min-h-0 flex-1 bg-[#282c34]">
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
          <div className="flex h-full flex-col items-center justify-center gap-3 bg-background text-sm text-muted-foreground">
            <div>
              No terminals in <span className="font-medium text-foreground">{basename(active.worktree)}</span>
            </div>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={hostState !== 'connected'}
                onClick={() => void newTerminalTab(active.hostId, active.worktree)}
              >
                <Plus />
                New terminal
              </Button>
              <Button
                size="sm"
                disabled={hostState !== 'connected'}
                onClick={() => useStore.setState({ newWorktree: { hostId: active.hostId } })}
              >
                <Sparkles />
                New worktree + agent
              </Button>
            </div>
          </div>
        )}
      </div>
    </main>
  )
}
