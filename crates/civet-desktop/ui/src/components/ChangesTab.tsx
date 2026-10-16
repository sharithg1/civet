// Changes tab of the right panel: the worktree's uncommitted changes
// (`git status` from the daemon, polled by the panel; see state/changes.ts) grouped Conflicts / Staged / Changes / Untracked, and a
// file's unified diff when one is picked. The daemon flags binary files and
// diffs too large to send; long diffs render in pages so a big file can't
// stall the panel.
import { useEffect, useMemo, useState } from 'react'
import { ArrowLeft, RefreshCw } from 'lucide-react'
import { CivetError } from '@/backend'
import type { GitChangeKind, GitDiffInfo, GitFileChange, GitStatusInfo } from '@/types'
import { useStore, wsKey } from '@/state/store'
import {
  CHANGES_BACKGROUND_POLL_MS,
  CHANGES_POLL_MS,
  changedFileCount,
  findChange,
  refreshChanges,
  useChanges,
  useChangesPolling,
  type ChangeRef,
  type ChangeSide
} from '@/state/changes'
import { parseUnifiedDiff, splitPath, type DiffLine } from '@/lib/diff'
import { cn } from '@/lib/utils'
import { IconButton } from './IconButton'

/** Diff lines rendered at first (and added per "Show more"). */
const DIFF_PAGE_LINES = 2000

const GROUPS: [ChangeSide, string][] = [
  ['conflicted', 'Conflicts'],
  ['staged', 'Staged'],
  ['unstaged', 'Changes'],
  ['untracked', 'Untracked']
]

const KIND: Record<GitChangeKind, [letter: string, label: string, className: string]> = {
  modified: ['M', 'Modified', 'text-status-wait'],
  added: ['A', 'Added', 'text-status-ok'],
  deleted: ['D', 'Deleted', 'text-status-err'],
  renamed: ['R', 'Renamed', 'text-status-run'],
  copied: ['C', 'Copied', 'text-status-run'],
  typeChanged: ['T', 'Type changed', 'text-status-wait'],
  untracked: ['U', 'Untracked', 'text-status-ok'],
  conflicted: ['!', 'Conflict', 'text-status-err']
}

export function ChangesTab({
  hostId,
  worktree,
  connected
}: {
  hostId: string
  worktree: string
  connected: boolean
}) {
  const entry = useChanges((s) => s.byWorktree[wsKey(hostId, worktree)])
  const [selected, setSelected] = useState<ChangeRef | null>(null)

  if (entry?.error?.code === 'unsupported') {
    return (
      <Pad>
        <Note>
          The civetd on this host is too old to report git changes. Restart its daemon to upgrade.
        </Note>
      </Pad>
    )
  }
  if (selected) {
    return (
      <DiffView
        hostId={hostId}
        worktree={worktree}
        target={selected}
        change={findChange(entry?.status, selected)}
        onBack={() => setSelected(null)}
      />
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
                    onClick={() => setSelected({ side, path: c.path, oldPath: c.oldPath })}
                  />
                ))}
              </div>
            )
          )}
          {status.truncated && (
            <Pad>
              <Note>Too many changed files; only the first ones are listed.</Note>
            </Pad>
          )}
        </>
      )}
    </div>
  )
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

function FileRow({ change, onClick }: { change: GitFileChange; onClick: () => void }) {
  const [name, dir] = splitPath(change.path)
  const [letter, label, color] = KIND[change.kind]
  const title = change.oldPath
    ? `${label}: ${change.oldPath} → ${change.path}`
    : `${label}: ${change.path}`
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className="mx-1.5 flex h-7 cursor-pointer items-center gap-2 rounded-md px-1.5 text-left text-sm hover:bg-hover"
    >
      <span className={cn('w-3 shrink-0 text-center font-mono text-xs', color)}>{letter}</span>
      <span
        className={cn(
          'shrink-0 truncate text-soft',
          change.kind === 'deleted' && 'text-quiet line-through'
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

function Counts({ additions, deletions }: { additions: number | null; deletions: number | null }) {
  if (additions == null || deletions == null) {
    return <span className="shrink-0 font-mono text-2xs text-dim">bin</span>
  }
  return (
    <span className="shrink-0 font-mono text-2xs">
      {additions > 0 && <span className="text-status-ok">+{additions}</span>}
      {additions > 0 && deletions > 0 && ' '}
      {deletions > 0 && <span className="text-status-err">−{deletions}</span>}
    </span>
  )
}

type DiffState = { key: string; diff: GitDiffInfo | null; error: CivetError | null }

function DiffView({
  hostId,
  worktree,
  target,
  change,
  onBack
}: {
  hostId: string
  worktree: string
  target: ChangeRef
  /** The file's current status entry; undefined once it no longer changes. */
  change: GitFileChange | undefined
  onBack: () => void
}) {
  const [state, setState] = useState<DiffState | null>(null)
  const [reload, setReload] = useState(0)
  const [shown, setShown] = useState(DIFF_PAGE_LINES)
  const key = `${target.side}\u0000${target.path}`
  // Refetch when the file's status (kind or line counts) changes.
  const sig = change ? `${change.kind}:${change.additions}:${change.deletions}` : 'gone'

  useEffect(() => {
    void sig
    void reload
    const b = useStore.getState().backend
    if (!b) return
    let live = true
    b.gitDiff(hostId, worktree, target.path, {
      oldPath: target.oldPath,
      staged: target.side === 'staged'
    }).then(
      (diff) => live && setState({ key, diff, error: null }),
      (err: unknown) => live && setState({ key, diff: null, error: CivetError.from(err) })
    )
    return () => {
      live = false
    }
  }, [hostId, worktree, target.path, target.oldPath, target.side, key, sig, reload])

  // Keep showing the previous diff of this file while a refetch runs.
  const current = state?.key === key ? state : null
  const diff = current?.diff ?? null
  const parsed = useMemo(() => (diff?.diff ? parseUnifiedDiff(diff.diff) : null), [diff])
  const [name, dir] = splitPath(target.path)
  const kind = change?.kind ?? (target.side === 'untracked' ? 'untracked' : null)

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex min-h-9 shrink-0 items-center gap-1.5 border-b border-border px-1.5 py-1">
        <IconButton label="Back to changed files" onClick={onBack}>
          <ArrowLeft className="size-3.5" />
        </IconButton>
        <div className="flex min-w-0 flex-1 flex-col" title={target.path}>
          <span className="truncate text-sm text-foreground">{name}</span>
          <span className="truncate text-2xs text-faint">
            {target.oldPath ? `from ${target.oldPath}` : dir || './'}
            {' · '}
            {target.side === 'staged' ? 'staged' : kind ? KIND[kind][1].toLowerCase() : 'unchanged'}
          </span>
        </div>
        {diff && <Counts additions={diff.additions} deletions={diff.deletions} />}
        <IconButton label="Reload diff" onClick={() => setReload((n) => n + 1)}>
          <RefreshCw className="size-3.5" />
        </IconButton>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {current?.error ? (
          <Pad>
            <div className="text-xs break-words text-status-err-text">{current.error.message}</div>
          </Pad>
        ) : !current ? (
          <Pad>
            <div className="text-xs text-faint">Loading diff…</div>
          </Pad>
        ) : diff?.binary ? (
          <Pad>
            <Note>Binary file; there is no text diff to show.</Note>
          </Pad>
        ) : diff?.tooLarge ? (
          <Pad>
            <Note>
              This diff is too large to show
              {diff.additions != null && diff.deletions != null
                ? ` (+${diff.additions} −${diff.deletions} lines)`
                : ''}
              . Open the file in a terminal tab instead.
            </Note>
          </Pad>
        ) : !parsed || parsed.hunks.length === 0 ? (
          <Pad>
            <div className="text-sm text-quiet">
              {metaLines(parsed?.headers ?? []).map((l) => (
                <div key={l} className="font-mono text-xs">
                  {l}
                </div>
              ))}
              {change ? '' : 'No changes.'}
            </div>
          </Pad>
        ) : (
          <DiffBody
            parsed={parsed}
            shown={shown}
            onMore={() => setShown((n) => n + DIFF_PAGE_LINES)}
          />
        )}
      </div>
    </div>
  )
}

/** Header lines worth showing when a diff has no hunks (mode change, pure rename). */
function metaLines(headers: string[]): string[] {
  return headers.filter((l) =>
    /^(old mode|new mode|similarity|rename|copy|deleted file|new file)/.test(l)
  )
}

function DiffBody({
  parsed,
  shown,
  onMore
}: {
  parsed: ReturnType<typeof parseUnifiedDiff>
  shown: number
  onMore: () => void
}) {
  let budget = shown
  return (
    <div className="w-max min-w-full pb-3 font-mono text-xs leading-[18px]">
      {parsed.hunks.map((h, i) => {
        if (budget <= 0) return null
        const lines = h.lines.slice(0, budget)
        budget -= lines.length
        return (
          <div key={i}>
            <div className="sticky left-0 w-[360px] max-w-full truncate bg-secondary/60 px-2 text-2xs leading-6 text-faint">
              {h.header}
            </div>
            {lines.map((l, j) => (
              <Line key={j} line={l} combined={parsed.combined} />
            ))}
          </div>
        )
      })}
      {parsed.lineCount > shown && (
        <div className="sticky left-0 w-[360px] max-w-full px-2 pt-2">
          <button
            type="button"
            onClick={onMore}
            className="h-[24px] cursor-pointer rounded-[5px] border border-border px-2.5 font-sans text-xs text-soft hover:bg-hover"
          >
            Show more ({parsed.lineCount - shown} lines left)
          </button>
        </div>
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

function Line({ line, combined }: { line: DiffLine; combined: boolean }) {
  const bg =
    line.kind === 'add' ? 'bg-status-ok/12' : line.kind === 'del' ? 'bg-status-err/12' : undefined
  const mark = line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '
  if (line.kind === 'note') {
    return <div className="pl-[88px] text-2xs text-faint italic">{line.text}</div>
  }
  return (
    <div className={cn('flex', bg)}>
      {!combined && <Gutter n={line.oldNo} />}
      <Gutter n={line.newNo} />
      <span
        className={cn(
          'w-4 shrink-0 text-center select-none',
          line.kind === 'add'
            ? 'text-status-ok'
            : line.kind === 'del'
              ? 'text-status-err'
              : 'text-dim'
        )}
      >
        {mark}
      </span>
      <span className="pr-3 whitespace-pre text-body">{line.text || ' '}</span>
    </div>
  )
}

function Gutter({ n }: { n: number | null }) {
  return <span className="w-9 shrink-0 pr-1.5 text-right text-dim select-none">{n ?? ''}</span>
}

function Pad({ children }: { children: React.ReactNode }) {
  return <div className="flex flex-col gap-2 px-3 py-2">{children}</div>
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded-[5px] border border-dashed border-border px-2 py-1.5 text-xs leading-normal text-faint">
      {children}
    </div>
  )
}
