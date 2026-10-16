// A diff tab's contents (opened from the Changes tab of the right panel): one
// file's unified diff with old/new line numbers. The daemon flags binary
// files and diffs too large to send; long diffs render in pages so a big file
// can't stall the window. The worktree's status is polled while the tab
// shows, and the diff is fetched again whenever the file's entry changes or a
// git action runs in the worktree.
import { useEffect, useMemo, useState } from 'react'
import { Copy, RotateCw } from 'lucide-react'
import { CivetError } from '@/backend'
import type { GitChangeKind, GitDiffInfo } from '@/types'
import { pushToast, useStore, wsKey, type DiffTarget } from '@/state/store'
import { CHANGES_POLL_MS, findChange, useChanges, useChangesPolling } from '@/state/changes'
import { parseUnifiedDiff, type DiffLine, type ParsedDiff } from '@/lib/diff'
import { cn } from '@/lib/utils'
import { useWorktreeInfo } from './chrome'
import { IconButton } from './IconButton'

/** Diff lines rendered at first (and added per "Show more"). */
const DIFF_PAGE_LINES = 3000

export const CHANGE_KIND: Record<
  GitChangeKind,
  [letter: string, label: string, className: string]
> = {
  modified: ['M', 'Modified', 'text-status-wait'],
  added: ['A', 'Added', 'text-status-ok'],
  deleted: ['D', 'Deleted', 'text-status-err'],
  renamed: ['R', 'Renamed', 'text-status-run'],
  copied: ['C', 'Copied', 'text-status-run'],
  typeChanged: ['T', 'Type changed', 'text-status-wait'],
  untracked: ['U', 'Untracked', 'text-status-ok'],
  conflicted: ['!', 'Conflict', 'text-status-err']
}

/** `+12 −3`, or `bin` without counts. */
export function Counts({
  additions,
  deletions
}: {
  additions: number | null
  deletions: number | null
}) {
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

type DiffState = { diff: GitDiffInfo | null; error: CivetError | null }

export function DiffView({
  hostId,
  worktree,
  path,
  target
}: {
  hostId: string
  worktree: string
  path: string
  target: DiffTarget
}) {
  useChangesPolling(hostId, worktree, CHANGES_POLL_MS)
  const change = useChanges((s) =>
    findChange(s.byWorktree[wsKey(hostId, worktree)]?.status, { ...target, path })
  )
  // Bumped by every git action in the worktree (stage, discard…): fetch again.
  const epoch = useChanges((s) => s.diffEpoch[wsKey(hostId, worktree)] ?? 0)
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  const root = useWorktreeInfo(hostId, worktree)?.info.path
  const [state, setState] = useState<DiffState | null>(null)
  const [reload, setReload] = useState(0)
  const [shown, setShown] = useState(DIFF_PAGE_LINES)
  // Fetch again when the file's status (kind or line counts) changes.
  const sig = change ? `${change.kind}:${change.additions}:${change.deletions}` : 'gone'

  useEffect(() => {
    void sig
    void reload
    void epoch
    const b = useStore.getState().backend
    if (!b || !connected) return
    let live = true
    b.gitDiff(hostId, worktree, path, {
      oldPath: target.oldPath,
      staged: target.side === 'staged'
    }).then(
      (diff) => live && setState({ diff, error: null }),
      (err: unknown) => live && setState({ diff: null, error: CivetError.from(err) })
    )
    return () => {
      live = false
    }
  }, [hostId, worktree, path, target.oldPath, target.side, sig, reload, epoch, connected])

  const diff = state?.diff ?? null
  const parsed = useMemo(() => (diff?.diff ? parseUnifiedDiff(diff.diff) : null), [diff])
  const kind = change?.kind ?? (target.side === 'untracked' ? 'untracked' : null)
  const copyPath = () => {
    const text = root ? `${root.replace(/\/+$/, '')}/${path}` : path
    void navigator.clipboard?.writeText(text).catch(() => {})
    pushToast({ kind: 'info', title: `Copied ${text}` }, 3200)
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-2 border-b border-border pr-1.5 pl-3">
        <span className="min-w-0 truncate font-mono text-xs text-soft" title={path}>
          {target.oldPath ? `${target.oldPath} → ${path}` : path}
        </span>
        <span className="shrink-0 text-xs text-quiet">
          {target.side === 'staged' ? 'Staged' : kind ? CHANGE_KIND[kind][1] : 'No longer changed'}
        </span>
        <span className="flex-1" />
        {diff && <Counts additions={diff.additions} deletions={diff.deletions} />}
        <IconButton label="Reload" disabled={!connected} onClick={() => setReload((n) => n + 1)}>
          <RotateCw />
        </IconButton>
        <IconButton label="Copy path" onClick={copyPath}>
          <Copy />
        </IconButton>
      </div>
      {state?.error ? (
        <Message error>{state.error.message}</Message>
      ) : !state ? (
        <Message>{connected ? 'Loading…' : 'Not connected.'}</Message>
      ) : diff?.binary ? (
        <Message>Binary file; there is no text diff to show.</Message>
      ) : diff?.tooLarge ? (
        <Message>
          This diff is too large to show
          {diff.additions != null && diff.deletions != null
            ? ` (+${diff.additions} −${diff.deletions} lines)`
            : ''}
          .
        </Message>
      ) : !parsed || parsed.hunks.length === 0 ? (
        <Message>{metaLines(parsed?.headers ?? []).join(' · ') || 'No changes.'}</Message>
      ) : (
        <DiffBody
          parsed={parsed}
          shown={shown}
          onMore={() => setShown((n) => n + DIFF_PAGE_LINES)}
        />
      )}
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
  parsed: ParsedDiff
  shown: number
  onMore: () => void
}) {
  let budget = shown
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      <div className="min-w-max pb-3 font-mono text-sm leading-[20px] select-text">
        {parsed.hunks.map((h, i) => {
          if (budget <= 0) return null
          const lines = h.lines.slice(0, budget)
          budget -= lines.length
          return (
            <div key={`${i}:${h.header}`}>
              <div className="sticky left-0 bg-secondary/60 px-3 font-mono text-xs leading-7 text-faint">
                {h.header}
              </div>
              {lines.map((l, j) => (
                <Line key={`${i}:${j}`} line={l} combined={parsed.combined} />
              ))}
            </div>
          )
        })}
        {parsed.lineCount > shown && (
          <div className="sticky left-0 px-3 pt-3">
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
    </div>
  )
}

function Line({ line, combined }: { line: DiffLine; combined: boolean }) {
  if (line.kind === 'note') {
    return (
      <div className={cn('text-xs text-faint italic', combined ? 'pl-[72px]' : 'pl-[124px]')}>
        {line.text}
      </div>
    )
  }
  const add = line.kind === 'add'
  const del = line.kind === 'del'
  return (
    <div className={cn('flex', add && 'bg-status-ok/12', del && 'bg-status-err/12')}>
      {!combined && <Gutter n={line.oldNo} />}
      <Gutter n={line.newNo} />
      <span
        className={cn(
          'w-5 shrink-0 text-center select-none',
          add ? 'text-status-ok' : del ? 'text-status-err' : 'text-dim'
        )}
      >
        {add ? '+' : del ? '-' : ' '}
      </span>
      <span className="pr-4 whitespace-pre text-soft">{line.text || ' '}</span>
    </div>
  )
}

function Gutter({ n }: { n: number | null }) {
  return <span className="w-[52px] shrink-0 pr-2 text-right text-dim select-none">{n ?? ''}</span>
}

function Message({ children, error }: { children: React.ReactNode; error?: boolean }) {
  return (
    <div
      className={cn(
        'grid flex-1 place-items-center px-6 text-center text-sm',
        error ? 'text-status-err' : 'text-quiet'
      )}
    >
      <div className="max-w-[420px]">{children}</div>
    </div>
  )
}
