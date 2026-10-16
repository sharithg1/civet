// Changes tab of the right panel: the worktree's uncommitted changes
// (`git status` from the daemon, polled by the panel; see state/changes.ts)
// grouped Conflicts / Staged / Changes / Untracked. Picking a file opens its
// diff in a tab of the workspace (DiffView.tsx), like the Files tab opens
// files.
import { RefreshCw } from 'lucide-react'
import type { GitFileChange, GitStatusInfo } from '@/types'
import { openDiffTab, useStore, wsKey } from '@/state/store'
import {
  CHANGES_BACKGROUND_POLL_MS,
  CHANGES_POLL_MS,
  changedFileCount,
  refreshChanges,
  useChanges,
  useChangesPolling,
  type ChangeSide
} from '@/state/changes'
import { splitPath } from '@/lib/diff'
import { cn } from '@/lib/utils'
import { CHANGE_KIND, Counts } from './DiffView'
import { IconButton } from './IconButton'

const GROUPS: [ChangeSide, string][] = [
  ['conflicted', 'Conflicts'],
  ['staged', 'Staged'],
  ['unstaged', 'Changes'],
  ['untracked', 'Untracked']
]

export function ChangesTab({
  hostId,
  worktree,
  connected
}: {
  hostId: string
  worktree: string
  connected: boolean
}) {
  const key = wsKey(hostId, worktree)
  const entry = useChanges((s) => s.byWorktree[key])
  // The diff tab showing, to highlight its row.
  const open = useStore((s) => {
    const ws = s.workspaces[key]
    const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
    return tab?.kind === 'file' && tab.diff ? `${tab.diff.side}\u0000${tab.path}` : null
  })

  if (entry?.error?.code === 'unsupported') {
    return (
      <div className="px-3 py-2">
        <Note>
          The civetd on this host is too old to report git changes. Restart its daemon to upgrade.
        </Note>
      </div>
    )
  }
  const status = entry?.status
  return (
    <div className="min-h-0 flex-1 overflow-y-auto pb-3">
      <ListHeader
        status={status}
        onRefresh={() => void refreshChanges(hostId, worktree)}
        connected={connected}
      />
      {entry?.error && (
        <div className="px-3 pb-2 text-xs break-words text-status-err-text">
          {entry.error.message}
        </div>
      )}
      {!status ? (
        !entry?.error && (
          <div className="px-3 text-xs text-faint">
            {connected ? 'Reading git status…' : 'Not connected.'}
          </div>
        )
      ) : changedFileCount(status) === 0 ? (
        <div className="px-3 text-sm text-quiet">No uncommitted changes.</div>
      ) : (
        <>
          {GROUPS.map(([side, title]) =>
            status[side].length === 0 ? null : (
              <div key={side} className="flex flex-col pb-1">
                <div className="flex h-7 items-center gap-1.5 px-3 text-xs text-faint">
                  {title} <span className="text-dim">· {status[side].length}</span>
                </div>
                {status[side].map((c) => (
                  <FileRow
                    key={c.path}
                    change={c}
                    active={open === `${side}\u0000${c.path}`}
                    onClick={() =>
                      openDiffTab(hostId, worktree, c.path, { side, oldPath: c.oldPath })
                    }
                  />
                ))}
              </div>
            )
          )}
          {status.truncated && (
            <div className="px-3 py-2">
              <Note>Too many changed files; only the first ones are listed.</Note>
            </div>
          )}
        </>
      )}
    </div>
  )
}

/** The tab label's changed-file count; polls the worktree's status meanwhile. */
export function ChangesCount({
  hostId,
  worktree,
  showing
}: {
  hostId: string
  worktree: string
  /** The Changes tab is the one showing: poll often. */
  showing: boolean
}) {
  useChangesPolling(hostId, worktree, showing ? CHANGES_POLL_MS : CHANGES_BACKGROUND_POLL_MS)
  const count = useChanges((s) => changedFileCount(s.byWorktree[wsKey(hostId, worktree)]?.status))
  return count > 0 ? <span className="font-mono text-xs text-quiet">{count}</span> : null
}

function ListHeader({
  status,
  onRefresh,
  connected
}: {
  status: GitStatusInfo | null | undefined
  onRefresh: () => void
  connected: boolean
}) {
  const count = changedFileCount(status)
  let adds = 0
  let dels = 0
  for (const side of ['conflicted', 'staged', 'unstaged', 'untracked'] as const) {
    for (const c of status?.[side] ?? []) {
      adds += c.additions ?? 0
      dels += c.deletions ?? 0
    }
  }
  return (
    <div className="flex h-9 items-center gap-2 px-3 text-sm">
      <span className="text-soft">
        {status ? `${count} changed file${count === 1 ? '' : 's'}` : 'Changes'}
      </span>
      {count > 0 && <Counts additions={adds} deletions={dels} />}
      <IconButton label="Refresh" className="ml-auto" disabled={!connected} onClick={onRefresh}>
        <RefreshCw className="size-3.5" />
      </IconButton>
    </div>
  )
}

function FileRow({
  change,
  active,
  onClick
}: {
  change: GitFileChange
  active: boolean
  onClick: () => void
}) {
  const [name, dir] = splitPath(change.path)
  const [letter, label, color] = CHANGE_KIND[change.kind]
  const title = change.oldPath
    ? `${label}: ${change.oldPath} → ${change.path}`
    : `${label}: ${change.path}`
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className={cn(
        'mx-1.5 flex h-7 cursor-pointer items-center gap-2 rounded-md px-1.5 text-left text-sm',
        active ? 'bg-card' : 'hover:bg-hover'
      )}
    >
      <span className={cn('w-3 shrink-0 text-center font-mono text-xs', color)}>{letter}</span>
      <span
        className={cn(
          'shrink-0 truncate',
          change.kind === 'deleted' ? 'text-quiet line-through' : 'text-soft'
        )}
      >
        {name}
      </span>
      <span className="min-w-0 flex-1 truncate text-xs text-dim" dir="rtl">
        {/* rtl: long directories lose their start, not the part next to the name */}
        <bdi>{dir}</bdi>
      </span>
      <Counts additions={change.additions} deletions={change.deletions} />
    </button>
  )
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded-[5px] border border-dashed border-border px-2 py-1.5 text-xs leading-normal text-faint">
      {children}
    </div>
  )
}
