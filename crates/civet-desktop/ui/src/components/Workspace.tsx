// Main area for the selected worktree: connection banner and the active
// tab's split layout, or a file or diff tab's contents (tabs live in the TitleBar),
// or the host stats view while it is open (StatsView.tsx). Only the active tab is
// mounted; switching tabs detaches the hidden panes and re-attaching replays
// a Full (cheap in the mosh model, and it keeps WebGL contexts bounded).
// While the worktree is being removed its panes are inert under a
// "Removing…" veil; the rest of the app stays usable.
import { isFolderWorktree, useStore, wsKey as makeWsKey } from '@/state/store'
import { leaves } from '@/layout/pane-tree'
import { Spinner } from '@/components/ui/spinner'
import { Kbd } from './chrome'
import { ConnectionBanner } from './ConnectionBanner'
import { DiffView } from './DiffView'
import { FileView } from './FileView'
import { SplitLayout } from './SplitLayout'
import { StatsView } from './StatsView'

export function Workspace() {
  const active = useStore((s) => s.active)
  const key = active ? makeWsKey(active.hostId, active.worktree) : ''
  const ws = useStore((s) => (key ? s.workspaces[key] : undefined))
  const removing = useStore((s) => !!key && !!s.removing[key])
  const folder = useStore((s) => !!s.active && isFolderWorktree(s.active.hostId, s.active.worktree))
  const statsOpen = useStore((s) => s.statsOpen)

  if (!active) {
    return (
      <main className="grid min-h-0 min-w-0 flex-1 place-items-center bg-background text-sm text-quiet">
        <div className="flex flex-col items-center gap-2">
          Select a worktree
          <span className="flex items-center gap-1.5 text-dim">
            <Kbd>⌘K</Kbd> to switch
          </span>
        </div>
      </main>
    )
  }
  const tab = ws?.tabs.find((t) => t.id === ws?.activeTab)
  return (
    <main className="flex min-h-0 min-w-0 flex-1 flex-col bg-background">
      <ConnectionBanner hostId={active.hostId} />
      {/* Why: `inert` also blurs a focused terminal, so keystrokes can't
          reach a pane that is about to be killed. */}
      <div
        className="relative min-h-0 flex-1"
        inert={removing}
        data-removing={removing ? 'true' : undefined}
      >
        {statsOpen ? (
          <StatsView hostId={active.hostId} worktree={active.worktree} />
        ) : tab?.kind === 'file' && tab.diff ? (
          <DiffView
            key={tab.id}
            hostId={active.hostId}
            worktree={active.worktree}
            path={tab.path}
            target={tab.diff}
          />
        ) : tab?.kind === 'file' ? (
          <FileView
            key={tab.id}
            hostId={active.hostId}
            worktree={active.worktree}
            path={tab.path}
          />
        ) : tab ? (
          <SplitLayout
            key={tab.id}
            hostId={active.hostId}
            wsKey={key}
            tabId={tab.id}
            node={tab.root}
            focused={tab.focused}
            multiPane={leaves(tab.root).length > 1}
          />
        ) : folder ? (
          <div className="grid h-full place-items-center text-sm text-quiet">
            <div className="flex flex-col items-center gap-2">
              No files open
              <button
                type="button"
                onClick={() => useStore.setState({ rightPanelOpen: true, rightTab: 'files' })}
                className="cursor-pointer text-dim hover:text-soft"
              >
                Open one from the Files panel
              </button>
            </div>
          </div>
        ) : (
          <div className="grid h-full place-items-center text-sm text-quiet">
            <div className="flex flex-col items-center gap-2">
              No tabs open
              <span className="flex items-center gap-1.5 text-dim">
                <Kbd>⌥C</Kbd> Claude Code · <Kbd>⌥X</Kbd> Codex · <Kbd>⌥T</Kbd> Terminal
              </span>
            </div>
          </div>
        )}
        {removing && (
          <div className="absolute inset-0 z-20 flex items-center justify-center gap-2 bg-background/70 text-sm text-faint">
            <Spinner className="size-3.5" />
            Removing this worktree…
          </div>
        )}
      </div>
    </main>
  )
}
