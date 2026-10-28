// "Clean up worktrees": the stale worktrees of one project (branch merged,
// remote branch deleted, PR merged or closed) with why each looks stale and
// what stands in the way. Opening it fetches (`--prune`) and asks gh, so the
// answer is current. Uncommitted changes, a working agent or a lock block a
// row (remove those one at a time, where "Remove anyway" is offered);
// unpushed commits that did not land are listed but left unchecked.
// Removing closes the dialog and runs the removals one after another in the
// background, through the same path as "Remove worktree…".
import { useEffect, useMemo, useState } from 'react'
import { Brush, GitMerge, TriangleAlert } from 'lucide-react'
import { CivetError } from '@/backend'
import type { RepoLifecycle, StaleReason, WorktreeLifecycle } from '@/types'
import { useStore, wtKey } from '@/state/store'
import {
  STALE_LABEL,
  cleanUpWorktrees,
  cleanupCandidates,
  refreshLifecycle,
  useLifecycle,
  type CleanupCandidate
} from '@/state/lifecycle'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Spinner } from '@/components/ui/spinner'

export function CleanupWorktreesDialog() {
  const req = useLifecycle((s) => s.cleanup)
  const close = () => useLifecycle.setState({ cleanup: null })
  return (
    <Dialog open={req !== null} onOpenChange={(o) => !o && close()}>
      <DialogContent
        showCloseButton={false}
        className="w-[560px] max-w-[calc(100vw-32px)] gap-3 sm:max-w-[560px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        {req && (
          <Body
            key={`${req.hostId}:${req.repo}`}
            hostId={req.hostId}
            repo={req.repo}
            close={close}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function reasonText(r: StaleReason, w: WorktreeLifecycle, base: string | null): string {
  if (r === 'merged') return `Merged into ${base ?? 'the default branch'}`
  if ((r === 'pullRequestMerged' || r === 'pullRequestClosed') && w.pullRequest) {
    return `PR #${w.pullRequest.number} ${r === 'pullRequestMerged' ? 'merged' : 'closed'}`
  }
  return STALE_LABEL[r]
}

function Body({ hostId, repo, close }: { hostId: string; repo: string; close: () => void }) {
  const name = useStore((s) => s.repos[hostId]?.find((r) => r.id === repo)?.name ?? repo)
  const worktrees = useStore((s) => s.worktrees[wtKey(hostId, repo)])
  const panes = useStore((s) => s.panes[hostId])
  const [review, setReview] = useState<RepoLifecycle | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [picked, setPicked] = useState<Set<string> | null>(null)

  useEffect(() => {
    const b = useStore.getState().backend
    if (!b) return
    let live = true
    b.worktreeLifecycle(hostId, repo, { fetch: true, review: true }).then(
      (l) => live && setReview(l),
      (err: unknown) => live && setError(CivetError.from(err).message)
    )
    return () => {
      live = false
    }
  }, [hostId, repo])

  const candidates = useMemo(() => {
    if (!review) return []
    const locked = (id: string) => !!worktrees?.find((w) => w.id === id)?.locked
    return cleanupCandidates(review, panes, locked)
  }, [review, panes, worktrees])
  // Preselected once the review arrives; the user's picks after that.
  const selected =
    picked ?? new Set(candidates.filter((c) => c.preselect).map((c) => c.info.worktree))
  const chosen = candidates.filter((c) => !c.blocked && selected.has(c.info.worktree))
  const toggle = (id: string, on: boolean) => {
    const next = new Set(selected)
    if (on) next.add(id)
    else next.delete(id)
    setPicked(next)
  }

  function run() {
    const ids = chosen.map((c) => c.info.worktree)
    close()
    void cleanUpWorktrees(hostId, ids).finally(() => void refreshLifecycle(hostId, repo))
  }

  const notes = [
    review?.fetchError && `Could not fetch, so this may be out of date: ${review.fetchError}`,
    review?.pullRequestsError &&
      `No pull requests (gh: ${review.pullRequestsError}); merged and deleted branches still show.`
  ].filter((n): n is string => !!n)

  return (
    <>
      <DialogTitle className="flex items-center gap-2">
        <Brush className="size-4 text-quiet" />
        Clean up {name}
      </DialogTitle>
      <DialogDescription className="text-sm text-quiet">
        Worktrees whose branch was merged, whose remote branch was deleted, or whose pull request
        was merged or closed.
      </DialogDescription>
      {error ? (
        <p className="text-sm text-status-err-text">{error}</p>
      ) : !review ? (
        <div className="flex items-center gap-2 py-6 text-sm text-quiet">
          <Spinner className="size-3.5" />
          Fetching and checking pull requests…
        </div>
      ) : candidates.length === 0 ? (
        <p className="py-6 text-center text-sm text-quiet">
          Nothing to clean up: no worktree looks finished with.
        </p>
      ) : (
        <div
          role="list"
          className="-mx-1 flex max-h-[min(420px,60vh)] flex-col gap-px overflow-y-auto"
        >
          {candidates.map((c) => (
            <Row
              key={c.info.worktree}
              c={c}
              base={review.base}
              checked={!c.blocked && selected.has(c.info.worktree)}
              onChange={(on) => toggle(c.info.worktree, on)}
            />
          ))}
        </div>
      )}
      {notes.map((n) => (
        <p key={n} className="text-xs text-quiet">
          {n}
        </p>
      ))}
      <div className="mt-1 flex items-center justify-end gap-2">
        <Button type="button" variant="outline" size="md" onClick={close}>
          {candidates.length ? 'Cancel' : 'Close'}
        </Button>
        {candidates.length > 0 && (
          <Button
            type="button"
            size="md"
            variant="destructive"
            disabled={chosen.length === 0}
            onClick={run}
          >
            Remove {chosen.length} worktree{chosen.length === 1 ? '' : 's'}
          </Button>
        )}
      </div>
    </>
  )
}

function Row({
  c,
  base,
  checked,
  onChange
}: {
  c: CleanupCandidate
  base: string | null
  checked: boolean
  onChange: (on: boolean) => void
}) {
  const w = c.info
  const id = `cleanup-${w.worktree}`
  return (
    <div
      role="listitem"
      data-cleanup={w.worktree}
      className={cn(
        'flex gap-2.5 rounded-md px-2 py-1.5',
        c.blocked ? 'opacity-70' : 'hover:bg-hover'
      )}
    >
      <Checkbox
        id={id}
        checked={checked}
        disabled={!!c.blocked}
        onCheckedChange={(v) => onChange(v === true)}
        className="mt-0.5"
        aria-label={`Remove ${w.branch ?? w.worktree}`}
      />
      <label htmlFor={id} className="flex min-w-0 flex-1 cursor-default flex-col gap-1">
        <span className="flex min-w-0 items-center gap-2">
          <span className="min-w-0 truncate font-mono text-sm text-foreground">
            {w.branch ?? w.worktree}
          </span>
          {(w.ahead > 0 || w.behind > 0) && (
            <span className="shrink-0 font-mono text-2xs text-quiet">
              {w.ahead > 0 && `↑${w.ahead}`}
              {w.ahead > 0 && w.behind > 0 && ' '}
              {w.behind > 0 && `↓${w.behind}`}
            </span>
          )}
        </span>
        <span className="flex flex-wrap gap-1">
          {w.stale.map((r) => (
            <span
              key={r}
              className="inline-flex items-center gap-1 rounded-[4px] bg-hover-strong px-1.5 py-px text-2xs text-soft"
            >
              {(r === 'merged' || r === 'pullRequestMerged') && (
                <GitMerge className="size-3 text-pr-merged" />
              )}
              {reasonText(r, w, base)}
            </span>
          ))}
        </span>
        {(c.blocked || c.warnings.length > 0) && (
          <span className="flex flex-col gap-0.5 text-xs">
            {c.blocked && (
              <span className="flex items-center gap-1 text-status-err-text">
                <TriangleAlert className="size-3 shrink-0" />
                {c.blocked}: remove it on its own
              </span>
            )}
            {c.warnings.map((t) => (
              <span key={t} className="text-quiet">
                {t}
              </span>
            ))}
          </span>
        )}
      </label>
    </div>
  )
}
