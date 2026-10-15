// Main area for the selected worktree: connection banner and the active
// tab's split layout (tabs live in the TitleBar). Only the active tab is
// mounted; switching tabs detaches the hidden panes and re-attaching replays
// a Full (cheap in the mosh model, and it keeps WebGL contexts bounded).
// While the worktree is being removed its panes are inert under a
// "Removing…" veil; the rest of the app stays usable.
import { useStore, wsKey as makeWsKey } from '@/state/store'
import { leaves } from '@/layout/pane-tree'
import { Spinner } from '@/components/ui/spinner'
import { ConnectionBanner } from './ConnectionBanner'
import { SplitLayout } from './SplitLayout'

export function Workspace() {
  const active = useStore((s) => s.active)
  const key = active ? makeWsKey(active.hostId, active.worktree) : ''
  const ws = useStore((s) => (key ? s.workspaces[key] : undefined))
  const removing = useStore((s) => !!key && !!s.removing[key])

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
      {/* Why: `inert` also blurs a focused terminal, so keystrokes can't
          reach a pane that is about to be killed. */}
      <div className="relative min-h-0 flex-1" inert={removing} data-removing={removing ? 'true' : undefined}>
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
        {removing && (
          <div className="absolute inset-0 z-20 flex items-center justify-center gap-2 bg-background/70 text-[12.5px] text-faint">
            <Spinner className="size-3.5" />
            Removing this worktree…
          </div>
        )}
      </div>
    </main>
  )
}
