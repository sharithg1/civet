// Activity tab of the right panel: "what did my agents do while I was away?"
// civetd records every agent turn in the worktree (from its status turning
// running until it stops, waits for input or exits) with the files that
// changed between the start and the end, commits included; see
// state/activity.ts. Newest first, grouped by day. Picking a file opens its
// change within that turn in a diff tab (DiffView.tsx → TurnDiffView).
import { useEffect, useState } from 'react'
import { History } from 'lucide-react'
import type { AgentTurn, GitFileChange } from '@/types'
import { openDiffTab, useStore, wsKey } from '@/state/store'
import { ACTIVITY_POLL_MS, useActivity, useActivityPolling } from '@/state/activity'
import { isTurn, TURN_STATUS, turnLength, turnLines, turnsByDay, turnTimes } from '@/lib/activity'
import { promptLabel } from '@/lib/worktree-view'
import { splitPath } from '@/lib/diff'
import { cn } from '@/lib/utils'
import { CHANGE_KIND, Counts } from './DiffView'
import { StatusDot } from './StatusDot'

/** Files listed per turn before "Show N more". */
const FILES_SHOWN = 6

export function ActivityTab({
  hostId,
  worktree,
  connected
}: {
  hostId: string
  worktree: string
  connected: boolean
}) {
  useActivityPolling(hostId, worktree, ACTIVITY_POLL_MS)
  const entry = useActivity((s) => s.byWorktree[wsKey(hostId, worktree)])
  const turns = entry?.activity?.turns ?? null
  const running = !!turns?.some((t) => t.endedMs == null)
  const now = useNow(running ? 1000 : null)
  const unsupported = entry?.error?.code === 'unsupported'

  return (
    <div className="min-h-0 flex-1 overflow-y-auto pb-3">
      {entry?.error && (
        <div className="px-3 pb-2 text-xs break-words text-status-err-text">
          {unsupported
            ? 'The daemon on this host is too old to record agent activity; restart it to upgrade.'
            : entry.error.message}
        </div>
      )}
      {!turns ? (
        !entry?.error && (
          <div className="px-3 text-xs text-faint">
            {connected ? 'Reading agent activity…' : 'Not connected.'}
          </div>
        )
      ) : turns.length === 0 ? (
        <div className="flex flex-col items-center gap-3 px-8 pt-16 text-center text-sm text-quiet">
          <History className="size-5 text-dim" />
          <div className="max-w-[260px] leading-relaxed">
            No agent activity yet. Each time an agent works here, its run shows up with how it ended
            and the files it changed.
          </div>
        </div>
      ) : (
        turnsByDay(turns, now).map((g) => (
          <div key={g.day} className="flex flex-col pb-1">
            <div className="sticky top-0 z-10 flex h-8 items-center bg-background px-3 text-xs text-quiet">
              {g.day}
            </div>
            {g.turns.map((t) => (
              <TurnRow key={t.id} hostId={hostId} worktree={worktree} turn={t} now={now} />
            ))}
          </div>
        ))
      )}
    </div>
  )
}

/** Date.now(), re-read every `periodMs` (null: never) so running turns' lengths tick. */
function useNow(periodMs: number | null): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (periodMs == null) return
    setNow(Date.now())
    const timer = setInterval(() => setNow(Date.now()), periodMs)
    return () => clearInterval(timer)
  }, [periodMs])
  return now
}

function TurnRow({
  hostId,
  worktree,
  turn,
  now
}: {
  hostId: string
  worktree: string
  turn: AgentTurn
  now: number
}) {
  const [all, setAll] = useState(false)
  const openTab = useStore((s) => s.workspaces[wsKey(hostId, worktree)]?.activeTab)
  const openPath = useStore((s) => {
    const ws = s.workspaces[wsKey(hostId, worktree)]
    const tab = ws?.tabs.find((x) => x.id === openTab)
    return tab?.kind === 'file' && tab.diff?.side === 'turn' && isTurn(tab.diff.turn, turn)
      ? tab.path
      : null
  })
  const [label, dot] = TURN_STATUS[turn.status]
  const prompt = promptLabel(turn.prompt)
  const { additions, deletions } = turnLines(turn)
  const files = all ? turn.files : turn.files.slice(0, FILES_SHOWN)
  const more = turn.files.length - files.length
  const snapshots =
    turn.fromTree && turn.toTree ? { id: turn.id, from: turn.fromTree, to: turn.toTree } : null
  const open = (f: GitFileChange, preview: boolean) => {
    if (!snapshots) return
    openDiffTab(
      hostId,
      worktree,
      f.path,
      { side: 'turn', oldPath: f.oldPath, turn: snapshots },
      { preview }
    )
  }

  return (
    <div className="mx-1.5 mb-1 flex flex-col rounded-md border border-border/60 py-1.5">
      <div
        className="flex min-w-0 items-center gap-2 px-2.5 text-sm"
        title={turn.prompt ?? undefined}
      >
        <span className="grid size-3.5 shrink-0 place-items-center">
          <StatusDot kind={dot} size={6} />
        </span>
        <span className={cn('min-w-0 truncate', prompt ? 'text-soft' : 'text-quiet italic')}>
          {prompt ?? 'Continued'}
        </span>
        <span className="ml-auto shrink-0 font-mono text-xs text-quiet">
          {turnLength(turn, now)}
        </span>
      </div>
      <div className="flex min-w-0 items-center gap-1.5 pl-8 pr-2.5 text-xs text-quiet">
        <span className="shrink-0">{turn.agent}</span>
        <span className="text-dim">·</span>
        <span className="shrink-0">{turnTimes(turn)}</span>
        <span className="text-dim">·</span>
        <span className={cn('shrink-0', turn.status === 'needsInput' && 'text-status-wait')}>
          {label}
        </span>
        {turn.files.length > 0 && (
          <span className="ml-auto shrink-0">
            <Counts additions={additions} deletions={deletions} />
          </span>
        )}
      </div>
      {turn.message && (
        <div className="line-clamp-2 pt-0.5 pr-2.5 pl-8 text-xs break-words text-faint">
          {turn.message}
        </div>
      )}
      {turn.endedMs != null && turn.files.length === 0 && (
        <div className="pt-0.5 pl-8 text-xs text-dim">
          {snapshots ? 'No files changed' : 'Files not tracked here'}
        </div>
      )}
      {files.length > 0 && (
        <div className="flex flex-col pt-1">
          {files.map((f) => (
            <TurnFile
              key={f.path}
              change={f}
              active={openPath === f.path}
              disabled={!snapshots}
              onClick={() => open(f, true)}
              onDoubleClick={() => open(f, false)}
            />
          ))}
          {(more > 0 || turn.filesTruncated) && (
            <button
              type="button"
              disabled={more === 0}
              onClick={() => setAll(true)}
              className="mx-1.5 h-6 cursor-pointer rounded-md px-1.5 pl-6.5 text-left text-xs text-quiet hover:bg-hover hover:text-soft disabled:cursor-default disabled:hover:bg-transparent"
            >
              {more > 0
                ? `Show ${more} more`
                : 'Too many files changed; only the first ones are listed.'}
            </button>
          )}
        </div>
      )}
    </div>
  )
}

function TurnFile({
  change,
  active,
  disabled,
  onClick,
  onDoubleClick
}: {
  change: GitFileChange
  active: boolean
  disabled: boolean
  /** A preview tab. */
  onClick: () => void
  /** A pinned tab. */
  onDoubleClick: () => void
}) {
  const [name, dir] = splitPath(change.path)
  const [letter, label, color] = CHANGE_KIND[change.kind]
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      onDoubleClick={onDoubleClick}
      title={
        change.oldPath ? `${label}: ${change.oldPath} → ${change.path}` : `${label}: ${change.path}`
      }
      className={cn(
        'mx-1.5 flex h-6 min-w-0 cursor-pointer items-center gap-2 rounded-md pr-1.5 pl-6.5 text-left text-sm disabled:cursor-default',
        active ? 'bg-card' : 'hover:bg-hover disabled:hover:bg-transparent'
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
