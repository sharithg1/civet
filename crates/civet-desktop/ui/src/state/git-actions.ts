// Git write actions of the Changes tab (stage, discard, commit, push…).
//
// One action at a time per worktree: `busy` labels it and the tab greys its
// controls meanwhile. Afterwards the worktree's status is fetched again right
// away and open diff tabs refetch (`diffEpoch`). git's refusals (push
// rejected, nothing to commit, conflicts) come back as error toasts with the
// lines of its message that matter. A discard returns a snapshot of what it
// overwrote; its toast offers Undo, which sends the snapshot back.

import { CivetError } from '@/backend'
import type { GitAction, GitDiscardSnapshot, GitFileChange } from '@/types'
import { pushToast, useStore, wsKey } from './store'
import { refreshChanges, useChanges, type ChangeSide } from './changes'

/** How long the "Discarded …" toast (and its Undo) stays. */
export const UNDO_TOAST_MS = 12_000

type Labels = {
  /** Shown while it runs ("Pushing…"). */
  busy: string
  /** Error toast title. */
  failed: string
  /** Info toast on success, if any. */
  done?: string
}

/**
 * Run one git action in a worktree. Resolves to the action's result (a
 * discard's snapshot, else null), or undefined if it failed (already
 * reported as a toast).
 */
export async function runGitAction(
  hostId: string,
  worktree: string,
  action: GitAction,
  labels: Labels
): Promise<GitDiscardSnapshot | null | undefined> {
  const b = useStore.getState().backend
  if (!b) return undefined
  const key = wsKey(hostId, worktree)
  setBusy(key, labels.busy)
  try {
    const result = await b.gitAction(hostId, worktree, action)
    if (labels.done) pushToast({ kind: 'info', title: labels.done }, 3200)
    return result
  } catch (err) {
    const e = CivetError.from(err)
    console.error(`[civet] ${labels.failed}`, e)
    pushToast({ kind: 'error', title: labels.failed, body: gitErrorMessage(e.message) }, 9000)
    return undefined
  } finally {
    setBusy(key, null)
    useChanges.setState((s) => ({
      diffEpoch: { ...s.diffEpoch, [key]: (s.diffEpoch[key] ?? 0) + 1 }
    }))
    void refreshChanges(hostId, worktree, 0, { force: true })
  }
}

function setBusy(key: string, label: string | null): void {
  useChanges.setState((s) => {
    const busy = { ...s.busy }
    if (label) busy[key] = label
    else delete busy[key]
    return { busy }
  })
}

/**
 * The part of a git error worth a toast: without the daemon's category prefix
 * and git's `hint:` lines, the `error:` / `fatal:` / `CONFLICT` / rejected
 * lines first.
 */
export function gitErrorMessage(message: string): string {
  const lines = message
    .replace(/^(git|invalid argument|i\/o): /, '')
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('hint:'))
  const key = lines.filter((l) => /^(error:|fatal:|CONFLICT|! \[|Automatic merge failed)/.test(l))
  return (key.length > 0 ? key : lines).map((l) => l.replace(/^(error|fatal): /, '')).join('\n')
}

/** Paths an action on these entries must name: both sides of a staged rename. */
export function changePaths(side: ChangeSide, changes: GitFileChange[]): string[] {
  const out = new Set<string>()
  for (const c of changes) {
    out.add(c.path)
    if (side === 'staged' && c.oldPath) out.add(c.oldPath)
  }
  return [...out]
}

const files = (n: number) => (n === 1 ? '1 file' : `${n} files`)

/** Stage (also marks conflicts resolved) or unstage entries of one side. */
export function stageChanges(
  hostId: string,
  worktree: string,
  side: ChangeSide,
  changes: GitFileChange[]
): Promise<unknown> {
  const paths = changePaths(side, changes)
  if (side === 'staged') {
    return runGitAction(
      hostId,
      worktree,
      { unstage: { paths } },
      {
        busy: 'Unstaging…',
        failed: 'Could not unstage'
      }
    )
  }
  return runGitAction(
    hostId,
    worktree,
    { stage: { paths } },
    {
      busy: side === 'conflicted' ? 'Marking resolved…' : 'Staging…',
      failed: side === 'conflicted' ? 'Could not mark resolved' : 'Could not stage'
    }
  )
}

/**
 * Discard entries: unstaged / untracked ones go back to the index (untracked
 * files are deleted), staged ones (`staged`, also "Discard all") to HEAD.
 * Offers Undo in a toast.
 */
export async function discardChanges(
  hostId: string,
  worktree: string,
  paths: string[],
  staged: boolean
): Promise<void> {
  if (paths.length === 0) return
  const snapshot = await runGitAction(
    hostId,
    worktree,
    { discard: { paths, staged } },
    {
      busy: 'Discarding…',
      failed: 'Could not discard changes'
    }
  )
  if (!snapshot || snapshot.entries.length === 0) return
  const n = snapshot.entries.length
  const only = n === 1 ? snapshot.entries[0] : null
  pushToast(
    {
      kind: 'info',
      title: only
        ? `${only.untracked ? 'Deleted' : 'Discarded changes to'} ${only.path.split('/').pop()}`
        : `Discarded changes to ${files(n)}`,
      action: {
        label: 'Undo',
        run: () =>
          void runGitAction(
            hostId,
            worktree,
            { restoreDiscarded: snapshot },
            {
              busy: 'Restoring…',
              failed: 'Could not undo the discard',
              done: `Restored ${files(n)}`
            }
          )
      }
    },
    UNDO_TOAST_MS
  )
}

/** The paths "Discard all" names: every change but conflicts. */
export function discardAllPaths(status: {
  staged: GitFileChange[]
  unstaged: GitFileChange[]
  untracked: GitFileChange[]
}): string[] {
  return [
    ...new Set([
      ...changePaths('staged', status.staged),
      ...changePaths('unstaged', status.unstaged),
      ...changePaths('untracked', status.untracked)
    ])
  ]
}

/** Commit; resolves true if it did. */
export async function commitChanges(
  hostId: string,
  worktree: string,
  opts: { message: string; amend: boolean; all: boolean }
): Promise<boolean> {
  const subject = opts.message.trim().split('\n')[0]
  const r = await runGitAction(
    hostId,
    worktree,
    { commit: opts },
    {
      busy: opts.amend ? 'Amending…' : 'Committing…',
      failed: opts.amend ? 'Could not amend the commit' : 'Could not commit',
      done: subject
        ? `${opts.amend ? 'Amended' : 'Committed'}: ${subject}`
        : 'Amended the last commit'
    }
  )
  return r !== undefined
}
