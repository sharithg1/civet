// Changes tab of the right panel: the worktree's uncommitted changes
// (`git status` from the daemon, polled by the panel; see state/changes.ts)
// grouped Conflicts / Staged / Changes / Untracked. Picking a file opens its
// diff in a tab of the workspace (DiffView.tsx), like the Files tab opens
// files; ⌘/⇧-click selects several rows of a group.
//
// Git actions (state/git-actions.ts): a commit box on top (⌘↵, Amend, "Commit
// all" when nothing is staged), the branch with ahead / behind and
// Pull / Push / Fetch, stage / unstage / discard per row, per selection and
// per group, conflict resolution, and a menu with stash, undo last commit and
// "Discard all". Discards are undone from their toast, so only "Discard all"
// asks first. Everything is greyed out while an action runs, and on a civetd
// too old for git actions.
import { useState } from 'react'
import {
  ArrowDown,
  ArrowLeftToLine,
  ArrowRightToLine,
  ArrowUp,
  Archive,
  ArchiveRestore,
  Check,
  CloudUpload,
  Ellipsis,
  GitBranch,
  Minus,
  Plus,
  RefreshCw,
  Trash2,
  Undo2
} from 'lucide-react'
import type { GitBranchInfo, GitFileChange, GitStatusInfo } from '@/types'
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
import {
  changePaths,
  commitChanges,
  discardAllPaths,
  discardChanges,
  runGitAction,
  stageChanges
} from '@/state/git-actions'
import { selectChange, selectedPaths, type ChangeSelection } from '@/lib/change-selection'
import { splitPath } from '@/lib/diff'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Spinner } from '@/components/ui/spinner'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'
import { CHANGE_KIND, Counts } from './DiffView'
import { IconButton } from './IconButton'

const GROUPS: [ChangeSide, string][] = [
  ['conflicted', 'Conflicts'],
  ['staged', 'Staged'],
  ['unstaged', 'Changes'],
  ['untracked', 'Untracked']
]

/** What the row / group buttons do to entries of one group. */
type GroupActions = {
  stage: (side: ChangeSide, changes: GitFileChange[]) => void
  discard: (side: ChangeSide, changes: GitFileChange[]) => void
  accept: (changes: GitFileChange[], side: 'ours' | 'theirs') => void
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
  const key = wsKey(hostId, worktree)
  const entry = useChanges((s) => s.byWorktree[key])
  const busy = useChanges((s) => s.busy[key] ?? null)
  const [sel, setSel] = useState<ChangeSelection>(null)
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
  const supported = entry?.actions === 'supported'
  const canAct = connected && supported && !busy
  const actions: GroupActions = {
    stage: (side, changes) => void stageChanges(hostId, worktree, side, changes),
    discard: (side, changes) =>
      void discardChanges(hostId, worktree, changePaths(side, changes), side === 'staged'),
    accept: (changes, side) =>
      void runGitAction(
        hostId,
        worktree,
        { acceptConflict: { paths: changes.map((c) => c.path), side } },
        { busy: 'Resolving…', failed: 'Could not resolve the conflict' }
      )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {status && supported && (
        <CommitBox
          hostId={hostId}
          worktree={worktree}
          status={status}
          branch={entry?.branch ?? null}
          disabled={!canAct}
        />
      )}
      {entry?.actions === 'unsupported' && (
        <div className="px-3 pt-2">
          <Note>
            This host's civetd is too old for git actions (stage, commit, push…). Restart its daemon
            to upgrade.
          </Note>
        </div>
      )}
      <ListHeader
        hostId={hostId}
        worktree={worktree}
        status={status}
        branch={entry?.branch ?? null}
        busy={busy}
        canAct={canAct}
        connected={connected}
      />
      <div className="min-h-0 flex-1 overflow-y-auto pb-3">
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
            {GROUPS.map(([side, title]) => {
              const list = status[side]
              if (list.length === 0) return null
              const order = list.map((c) => c.path)
              const picked = selectedPaths(sel, side, order)
              return (
                <div key={side} className="flex flex-col pb-1">
                  <GroupHeader
                    side={side}
                    title={title}
                    changes={list}
                    actions={actions}
                    disabled={!canAct}
                  />
                  {list.map((c) => (
                    <FileRow
                      key={c.path}
                      side={side}
                      change={c}
                      active={open === `${side}\u0000${c.path}`}
                      selected={picked.length > 1 && picked.includes(c.path)}
                      disabled={!canAct}
                      // A row of a multi-selection acts on the whole selection.
                      targets={
                        picked.length > 1 && picked.includes(c.path)
                          ? list.filter((x) => picked.includes(x.path))
                          : [c]
                      }
                      actions={actions}
                      onClick={(e) => {
                        const toggle = e.metaKey || e.ctrlKey
                        const range = e.shiftKey
                        setSel((s) => selectChange(s, side, order, c.path, { toggle, range }))
                        if (!toggle && !range) {
                          openDiffTab(
                            hostId,
                            worktree,
                            c.path,
                            { side, oldPath: c.oldPath },
                            { preview: true }
                          )
                        }
                      }}
                      onDoubleClick={() =>
                        openDiffTab(hostId, worktree, c.path, { side, oldPath: c.oldPath })
                      }
                    />
                  ))}
                </div>
              )
            })}
            {status.truncated && (
              <div className="px-3 py-2">
                <Note>Too many changed files; only the first ones are listed.</Note>
              </div>
            )}
          </>
        )}
      </div>
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

function CommitBox({
  hostId,
  worktree,
  status,
  branch,
  disabled
}: {
  hostId: string
  worktree: string
  status: GitStatusInfo
  branch: GitBranchInfo | null
  disabled: boolean
}) {
  const key = wsKey(hostId, worktree)
  const message = useChanges((s) => s.drafts[key] ?? '')
  const [amend, setAmend] = useState(false)
  const setMessage = (m: string) =>
    useChanges.setState((s) => ({ drafts: { ...s.drafts, [key]: m } }))
  const staged = status.staged.length
  const others = status.unstaged.length + status.untracked.length
  // Nothing staged: offer to commit everything, like VS Code.
  const all = !amend && staged === 0 && others > 0
  const empty = !message.trim()
  const blocked = disabled || (!amend && (empty || staged + others === 0))
  const label = amend ? 'Amend' : all ? 'Commit all' : 'Commit'
  const commit = async () => {
    if (blocked) return
    if (await commitChanges(hostId, worktree, { message, amend, all })) {
      setMessage('')
      setAmend(false)
    }
  }
  return (
    <div className="flex shrink-0 flex-col gap-1.5 border-b border-border px-3 pt-2 pb-2.5">
      <textarea
        aria-label="Commit message"
        value={message}
        onChange={(e) => setMessage(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
            e.preventDefault()
            void commit()
          }
        }}
        rows={2}
        placeholder={
          amend
            ? "Message (empty keeps the last commit's)"
            : `Message (${navigator.platform.startsWith('Mac') ? '⌘' : 'Ctrl+'}↵ to commit)`
        }
        className="field-sizing-content max-h-40 min-h-[52px] w-full resize-none rounded-[5px] border border-border bg-transparent px-2 py-1.5 text-sm text-soft outline-none placeholder:text-dim focus-visible:border-ring"
      />
      <div className="flex items-center gap-2">
        <label
          className={cn(
            'flex cursor-pointer items-center gap-1.5 text-xs text-quiet select-none',
            !branch?.head && 'pointer-events-none opacity-50'
          )}
        >
          <Checkbox
            checked={amend}
            disabled={!branch?.head}
            onCheckedChange={(v) => setAmend(v === true)}
            className="size-3.5"
          />
          Amend
        </label>
        <span className="flex-1" />
        <Button
          size="xs"
          disabled={blocked}
          onClick={() => void commit()}
          title={
            all
              ? `Nothing is staged: stage all ${others} change${others === 1 ? '' : 's'} and commit`
              : undefined
          }
        >
          <Check />
          {label}
        </Button>
      </div>
    </div>
  )
}

function ListHeader({
  hostId,
  worktree,
  status,
  branch,
  busy,
  canAct,
  connected
}: {
  hostId: string
  worktree: string
  status: GitStatusInfo | null | undefined
  branch: GitBranchInfo | null
  busy: string | null
  canAct: boolean
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
    <div className="flex shrink-0 flex-col">
      {branch && (
        <BranchBar
          hostId={hostId}
          worktree={worktree}
          branch={branch}
          status={status ?? null}
          busy={busy}
          canAct={canAct}
        />
      )}
      <div className="flex h-9 items-center gap-2 px-3 text-sm">
        <span className="text-soft">
          {status ? `${count} changed file${count === 1 ? '' : 's'}` : 'Changes'}
        </span>
        {count > 0 && <Counts additions={adds} deletions={dels} />}
        <IconButton
          label="Refresh"
          className="ml-auto"
          disabled={!connected}
          onClick={() => void refreshChanges(hostId, worktree, 0, { force: true })}
        >
          <RefreshCw className="size-3.5" />
        </IconButton>
      </div>
    </div>
  )
}

/** Branch, ahead / behind, Pull / Push / Fetch and the "more" menu. */
function BranchBar({
  hostId,
  worktree,
  branch,
  status,
  busy,
  canAct
}: {
  hostId: string
  worktree: string
  branch: GitBranchInfo
  status: GitStatusInfo | null
  busy: string | null
  canAct: boolean
}) {
  const name = branch.branch ?? 'detached HEAD'
  const run = (
    action: Parameters<typeof runGitAction>[2],
    labels: Parameters<typeof runGitAction>[3]
  ) => void runGitAction(hostId, worktree, action, labels)
  const push = () =>
    run('push', {
      busy: branch.upstream ? 'Pushing…' : 'Publishing…',
      failed: branch.upstream ? 'Push failed' : 'Could not publish the branch',
      done: branch.upstream ? `Pushed ${name}` : `Published ${name}`
    })
  const pull = () =>
    run('pull', { busy: 'Pulling…', failed: 'Pull failed', done: `Pulled ${branch.upstream}` })
  const fetch = () => run('fetch', { busy: 'Fetching…', failed: 'Fetch failed' })
  const changes = status ? discardAllPaths(status) : []
  // Files, as listed (a rename's source path is not one more).
  const files = new Set(
    [...(status?.staged ?? []), ...(status?.unstaged ?? []), ...(status?.untracked ?? [])].map(
      (c) => c.path
    )
  ).size
  const canPush = canAct && !!branch.branch && branch.hasRemote
  const discardAll = () =>
    useStore.setState({
      confirm: {
        title: 'Discard all changes?',
        body: `Every change in this worktree goes back to the last commit: ${files} file${files === 1 ? '' : 's'}, staged or not, and untracked files are deleted. Undo is offered right after.`,
        confirmLabel: 'Discard all',
        focusCancel: true,
        onConfirm: () => void discardChanges(hostId, worktree, changes, true)
      }
    })

  return (
    <div className="flex h-9 items-center gap-1 border-b border-border pr-1.5 pl-3 text-sm">
      <GitBranch className="size-3.5 shrink-0 text-faint" />
      <span className="min-w-0 truncate font-mono text-xs text-soft" title={name}>
        {name}
      </span>
      {branch.upstream && (branch.ahead > 0 || branch.behind > 0) && (
        <span
          className="shrink-0 font-mono text-2xs text-quiet"
          title={`${branch.ahead} to push, ${branch.behind} to pull (${branch.upstream})`}
        >
          {branch.ahead > 0 && `↑${branch.ahead}`}
          {branch.ahead > 0 && branch.behind > 0 && ' '}
          {branch.behind > 0 && `↓${branch.behind}`}
        </span>
      )}
      <span className="flex-1" />
      {busy ? (
        <span className="flex shrink-0 items-center gap-1.5 px-1 text-xs text-quiet">
          <Spinner className="size-3" />
          {busy}
        </span>
      ) : !branch.upstream && branch.branch && branch.hasRemote ? (
        <Button size="xs" variant="outline" disabled={!canPush} onClick={push}>
          <CloudUpload />
          Publish
        </Button>
      ) : branch.upstream ? (
        <>
          <IconButton
            label={branch.behind > 0 ? `Pull ${branch.behind} from ${branch.upstream}` : 'Pull'}
            disabled={!canAct}
            onClick={pull}
          >
            <ArrowDown />
          </IconButton>
          <IconButton
            label={branch.ahead > 0 ? `Push ${branch.ahead} to ${branch.upstream}` : 'Push'}
            disabled={!canPush || branch.ahead === 0}
            onClick={push}
          >
            <ArrowUp />
          </IconButton>
        </>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger
          aria-label="More git actions"
          disabled={!canAct}
          className="grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-muted-foreground outline-none hover:bg-hover-strong hover:text-foreground disabled:pointer-events-none disabled:opacity-50 data-[state=open]:bg-hover-strong"
        >
          <Ellipsis className="size-3.5" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" sideOffset={4} className="w-56">
          <DropdownMenuItem disabled={!branch.hasRemote} onSelect={fetch}>
            <RefreshCw />
            Fetch
          </DropdownMenuItem>
          <DropdownMenuItem disabled={!branch.upstream} onSelect={pull}>
            <ArrowDown />
            Pull
          </DropdownMenuItem>
          <DropdownMenuItem disabled={!canPush} onSelect={push}>
            <ArrowUp />
            {branch.upstream ? 'Push' : 'Publish branch'}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            disabled={!branch.canUndoCommit || !branch.head}
            onSelect={() =>
              branch.head &&
              run(
                { undoCommit: { head: branch.head } },
                {
                  busy: 'Undoing commit…',
                  failed: 'Could not undo the last commit',
                  done: 'Undid the last commit; its changes are staged'
                }
              )
            }
          >
            <Undo2 />
            Undo last commit
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={changes.length === 0}
            onSelect={() =>
              run('stashAll', {
                busy: 'Stashing…',
                failed: 'Could not stash',
                done: 'Stashed all changes'
              })
            }
          >
            <Archive />
            Stash all changes
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={branch.stashes === 0}
            onSelect={() =>
              run('stashPop', {
                busy: 'Popping stash…',
                failed: 'Could not pop the stash',
                done: 'Applied and dropped the latest stash'
              })
            }
          >
            <ArchiveRestore />
            Pop latest stash{branch.stashes > 0 ? ` (${branch.stashes})` : ''}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            variant="destructive"
            disabled={changes.length === 0}
            onSelect={discardAll}
          >
            <Trash2 />
            Discard all changes…
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

function GroupHeader({
  side,
  title,
  changes,
  actions,
  disabled
}: {
  side: ChangeSide
  title: string
  changes: GitFileChange[]
  actions: GroupActions
  disabled: boolean
}) {
  return (
    <div className="group/header flex h-7 items-center gap-1.5 pr-2.5 pl-3 text-xs text-faint">
      {title} <span className="text-dim">· {changes.length}</span>
      <span className="flex-1" />
      <div className="flex items-center opacity-0 group-hover/header:opacity-100 focus-within:opacity-100">
        <EntryActions side={side} changes={changes} actions={actions} disabled={disabled} all />
      </div>
    </div>
  )
}

/** The buttons of a row (or with `all`, of a group header). */
function EntryActions({
  side,
  changes,
  actions,
  disabled,
  all
}: {
  side: ChangeSide
  changes: GitFileChange[]
  actions: GroupActions
  disabled: boolean
  all?: boolean
}) {
  const n = changes.length
  // One file, a selection of `n`, or the whole group.
  const label = (one: string, many: (n: string) => string, every: string) =>
    all ? every : n > 1 ? many(`${n} files`) : one
  const btn = (text: string, icon: React.ReactNode, onClick: () => void) => (
    <IconButton
      label={text}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation()
        onClick()
      }}
    >
      {icon}
    </IconButton>
  )
  if (side === 'conflicted') {
    return (
      <>
        {btn(
          label('Accept ours', (f) => `Accept ours for ${f}`, 'Accept ours for all'),
          <ArrowLeftToLine />,
          () => actions.accept(changes, 'ours')
        )}
        {btn(
          label('Accept theirs', (f) => `Accept theirs for ${f}`, 'Accept theirs for all'),
          <ArrowRightToLine />,
          () => actions.accept(changes, 'theirs')
        )}
        {btn(
          label('Mark resolved', (f) => `Mark ${f} resolved`, 'Mark all resolved'),
          <Check />,
          () => actions.stage(side, changes)
        )}
      </>
    )
  }
  if (side === 'staged') {
    return (
      <>
        {btn(
          label('Discard changes', (f) => `Discard changes to ${f}`, 'Discard all changes'),
          <Undo2 />,
          () => actions.discard(side, changes)
        )}
        {btn(
          label('Unstage', (f) => `Unstage ${f}`, 'Unstage all'),
          <Minus />,
          () => actions.stage(side, changes)
        )}
      </>
    )
  }
  return (
    <>
      {side === 'untracked'
        ? btn(
            label('Delete', (f) => `Delete ${f}`, 'Delete all'),
            <Trash2 />,
            () => actions.discard(side, changes)
          )
        : btn(
            label('Discard changes', (f) => `Discard changes to ${f}`, 'Discard all changes'),
            <Undo2 />,
            () => actions.discard(side, changes)
          )}
      {btn(
        label('Stage', (f) => `Stage ${f}`, 'Stage all'),
        <Plus />,
        () => actions.stage(side, changes)
      )}
    </>
  )
}

function FileRow({
  side,
  change,
  active,
  selected,
  disabled,
  targets,
  actions,
  onClick,
  onDoubleClick
}: {
  side: ChangeSide
  change: GitFileChange
  active: boolean
  selected: boolean
  disabled: boolean
  /** What the row's buttons act on: the row, or the selection it is part of. */
  targets: GitFileChange[]
  actions: GroupActions
  /** Plain click: a preview tab; ⌘ / ⇧: change the selection. */
  onClick: (e: React.MouseEvent) => void
  /** A pinned tab. */
  onDoubleClick: () => void
}) {
  const [name, dir] = splitPath(change.path)
  const [letter, label, color] = CHANGE_KIND[change.kind]
  const title = change.oldPath
    ? `${label}: ${change.oldPath} → ${change.path}`
    : `${label}: ${change.path}`
  return (
    <div
      aria-selected={selected || active}
      className={cn(
        'group/row mx-1.5 flex h-7 items-center rounded-md',
        active ? 'bg-card' : selected ? 'bg-hover-strong' : 'hover:bg-hover'
      )}
    >
      <button
        type="button"
        onClick={onClick}
        onDoubleClick={onDoubleClick}
        title={title}
        className="flex h-full min-w-0 flex-1 cursor-pointer items-center gap-2 pl-1.5 text-left text-sm"
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
        <span className="pr-1.5 group-focus-within/row:hidden group-hover/row:hidden">
          <Counts additions={change.additions} deletions={change.deletions} />
        </span>
      </button>
      <div className="hidden shrink-0 items-center pr-1 group-focus-within/row:flex group-hover/row:flex">
        <EntryActions side={side} changes={targets} actions={actions} disabled={disabled} />
      </div>
    </div>
  )
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded-[5px] border border-dashed border-border px-2 py-1.5 text-xs leading-normal text-faint">
      {children}
    </div>
  )
}
