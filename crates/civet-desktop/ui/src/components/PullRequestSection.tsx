// The worktree branch's pull request (state/pull-requests.ts), under the
// branch bar of the Changes tab: number, title and state (opens it on
// GitHub), CI checks (expandable, each opens its run), review decision and
// comment counts, and the unresolved review threads (expandable, each opens
// the comment). Without a PR, "Create pull request" opens a form (title,
// description, base branch, draft) that pushes the branch first if needed
// and runs `gh pr create` on the host. The Branch tab shows the same card
// without the form (`branch` null).
import { useState } from 'react'
import {
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleMinus,
  CircleSlash,
  CircleX,
  ExternalLink,
  Eye,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestCreate,
  GitPullRequestDraft,
  MessageSquare
} from 'lucide-react'
import type { CheckState, GitBranchInfo, PullRequestDetail, PullRequestState } from '@/types'
import { wsKey } from '@/state/store'
import {
  checksSummary,
  createPullRequest,
  needsPush,
  openUrl,
  PR_DETAIL_POLL_MS,
  reviewSummary,
  sortedChecks,
  usePullRequestPolling,
  usePullRequests
} from '@/state/pull-requests'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Spinner } from '@/components/ui/spinner'
import { IconButton } from './IconButton'

export const PR_STATE = {
  open: { Icon: GitPullRequest, className: 'text-status-ok', label: 'Open' },
  draft: { Icon: GitPullRequestDraft, className: 'text-quiet', label: 'Draft' },
  merged: { Icon: GitMerge, className: 'text-pr-merged', label: 'Merged' },
  closed: { Icon: GitPullRequestClosed, className: 'text-status-err', label: 'Closed' }
} as const satisfies Record<PullRequestState, unknown>

export const CHECK_STATE = {
  passed: { Icon: CircleCheck, className: 'text-status-ok', label: 'Checks passed' },
  failed: { Icon: CircleX, className: 'text-status-err', label: 'Checks failing' },
  pending: { Icon: CircleDashed, className: 'text-status-wait', label: 'Checks running' },
  skipped: { Icon: CircleSlash, className: 'text-dim', label: 'Skipped' },
  cancelled: { Icon: CircleMinus, className: 'text-dim', label: 'Cancelled' }
} as const satisfies Record<CheckState, unknown>

export function PullRequestSection({
  hostId,
  worktree,
  branchName,
  defaultBranch,
  branch,
  connected,
  canAct
}: {
  hostId: string
  worktree: string
  /** The worktree's branch; null: detached (no PR). */
  branchName: string | null
  defaultBranch: string | null
  /** For the create form (push state); null: no form. */
  branch: GitBranchInfo | null
  connected: boolean
  canAct: boolean
}) {
  const key = wsKey(hostId, worktree)
  // The default branch has no PR of its own.
  const enabled = !!branchName && branchName !== defaultBranch
  usePullRequestPolling(hostId, worktree, PR_DETAIL_POLL_MS, { threads: true, enabled })
  const entry = usePullRequests((s) => s.byWorktree[key])
  const creating = usePullRequests((s) => !!s.creating[key])
  const [form, setForm] = useState(false)
  if (!enabled || !entry) return null

  const detail = entry.detail
  if (!detail && entry.error) {
    return (
      <div className="border-b border-border px-3 py-2 text-xs break-words text-faint">
        {entry.error.code === 'unsupported'
          ? "This host's civetd is too old to report pull requests. Restart its daemon to upgrade."
          : `Pull request status unavailable: ${prErrorMessage(entry.error.message)}`}
      </div>
    )
  }
  // A closed PR can be replaced by a new one; open / draft / merged cannot.
  const canCreate = !!branch?.hasRemote && (!detail || detail.pr.state === 'closed')
  return (
    <div className="flex shrink-0 flex-col border-b border-border">
      {detail && <PullRequestCard detail={detail} />}
      {canCreate &&
        (form && branch ? (
          <CreateForm
            hostId={hostId}
            worktree={worktree}
            branch={branch}
            defaultBranch={defaultBranch}
            disabled={!canAct || creating}
            creating={creating}
            onDone={() => setForm(false)}
          />
        ) : (
          <div className="flex h-9 items-center gap-2 px-3 text-sm">
            <span className="min-w-0 flex-1 truncate text-quiet">
              {detail ? 'Closed without merging' : 'No pull request'}
            </span>
            <Button
              size="xs"
              variant="outline"
              disabled={!connected || !canAct}
              onClick={() => setForm(true)}
            >
              <GitPullRequestCreate />
              Create pull request
            </Button>
          </div>
        ))}
      {!detail && !canCreate && (
        <div className="px-3 py-2 text-xs text-faint">No pull request.</div>
      )}
    </div>
  )
}

/** gh's message without the daemon's category prefix. */
function prErrorMessage(message: string): string {
  const m = message.replace(/^(git|spawn|i\/o): /, '')
  return m === 'gh not found' ? 'the GitHub CLI (gh) is not installed on the host' : m
}

function PullRequestCard({ detail }: { detail: PullRequestDetail }) {
  const { pr } = detail
  const state = PR_STATE[pr.state]
  const [showChecks, setShowChecks] = useState(detail.checksState === 'failed')
  const [showThreads, setShowThreads] = useState(true)
  const review = reviewSummary(detail)
  const live = pr.state === 'open' || pr.state === 'draft'
  const checks = detail.checksState ? CHECK_STATE[detail.checksState] : null
  return (
    <div className="flex flex-col pt-1.5 pb-1.5 text-sm">
      <div className="flex min-w-0 items-start gap-2 px-3">
        <state.Icon className={cn('mt-0.5 size-3.5 shrink-0', state.className)} />
        <button
          type="button"
          onClick={() => void openUrl(pr.url)}
          title={`Open #${pr.number} on GitHub`}
          className="min-w-0 flex-1 cursor-pointer text-left leading-snug text-soft hover:text-foreground"
        >
          <span className="font-mono text-xs text-quiet">#{pr.number}</span> {pr.title}
        </button>
        <IconButton label="Open on GitHub" onClick={() => void openUrl(pr.url)} className="-mt-0.5">
          <ExternalLink className="size-3.5" />
        </IconButton>
      </div>
      <div className="flex min-w-0 items-center gap-1.5 pl-[34px] pr-3 text-xs text-faint">
        <span className={state.className}>{state.label}</span>
        <span>·</span>
        <span className="min-w-0 truncate font-mono">
          {detail.base} ← {pr.branch}
        </span>
      </div>
      {checks && (
        <Row
          icon={<checks.Icon className={cn('size-3.5', checks.className)} />}
          expanded={showChecks}
          onToggle={() => setShowChecks((v) => !v)}
          label={`${checks.label}: ${checksSummary(detail)}`}
        >
          {checksSummary(detail)}
        </Row>
      )}
      {showChecks && checks && (
        <div className="flex flex-col pb-1">
          {sortedChecks(detail).map((c, i) => {
            const s = CHECK_STATE[c.state]
            return (
              <button
                key={`${c.name}\u0000${i}`}
                type="button"
                disabled={!c.url}
                onClick={() => c.url && void openUrl(c.url)}
                title={`${c.name}: ${c.state}${c.url ? ' (click to open)' : ''}`}
                className="mx-1.5 flex h-6 min-w-0 cursor-pointer items-center gap-2 rounded-md pr-1.5 pl-[26px] text-left text-xs text-quiet enabled:hover:bg-hover enabled:hover:text-soft disabled:cursor-default"
              >
                <s.Icon className={cn('size-3 shrink-0', s.className)} />
                <span className="min-w-0 truncate">{c.name}</span>
              </button>
            )
          })}
        </div>
      )}
      {(review || detail.comments > 0) && (
        <Row
          icon={
            detail.reviewDecision === 'approved' ? (
              <CircleCheck className="size-3.5 text-status-ok" />
            ) : detail.reviewDecision === 'changesRequested' ? (
              <CircleAlert className="size-3.5 text-status-err" />
            ) : (
              <Eye className="size-3.5 text-faint" />
            )
          }
        >
          {[review, detail.comments > 0 && plural(detail.comments, 'comment')]
            .filter(Boolean)
            .join(' · ')}
        </Row>
      )}
      {live && detail.unresolvedThreads > 0 && (
        <Row
          icon={<MessageSquare className="size-3.5 text-status-wait" />}
          expanded={detail.threads.length > 0 ? showThreads : undefined}
          onToggle={() => setShowThreads((v) => !v)}
          label="Unresolved review comments"
        >
          {plural(detail.unresolvedThreads, 'unresolved comment')}
        </Row>
      )}
      {live && showThreads && detail.threads.length > 0 && (
        <div className="flex flex-col gap-0.5 pb-1">
          {detail.threads.map((t) => (
            <button
              key={t.url}
              type="button"
              onClick={() => void openUrl(t.url)}
              title="Open the comment on GitHub"
              className="mx-1.5 flex min-w-0 cursor-pointer flex-col gap-0.5 rounded-md py-1 pr-1.5 pl-[26px] text-left hover:bg-hover"
            >
              <span className="flex min-w-0 items-center gap-1.5 text-xs">
                <span className="min-w-0 truncate font-mono text-soft" dir="rtl">
                  <bdi>
                    {t.path}
                    {t.line != null ? `:${t.line}` : ''}
                  </bdi>
                </span>
                {t.outdated && (
                  <span className="shrink-0 rounded-[4px] bg-secondary px-1 text-2xs text-quiet">
                    outdated
                  </span>
                )}
                <span className="ml-auto shrink-0 text-faint">
                  {t.author}
                  {t.comments > 1 ? ` +${t.comments - 1}` : ''}
                </span>
              </span>
              <span className="line-clamp-2 text-xs leading-snug break-words text-quiet">
                {t.body}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/** A summary line of the card; with `onToggle`, a disclosure. */
function Row({
  icon,
  expanded,
  onToggle,
  label,
  children
}: {
  icon: React.ReactNode
  expanded?: boolean
  onToggle?: () => void
  label?: string
  children: React.ReactNode
}) {
  const content = (
    <>
      <span className="grid size-3.5 shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {expanded != null &&
        (expanded ? (
          <ChevronDown className="size-3.5 shrink-0 text-dim" />
        ) : (
          <ChevronRight className="size-3.5 shrink-0 text-dim" />
        ))}
    </>
  )
  const cls =
    'mx-1.5 flex h-6 min-w-0 items-center gap-2 rounded-md px-1.5 pl-[22px] text-xs text-quiet'
  return expanded != null && onToggle ? (
    <button
      type="button"
      aria-expanded={expanded}
      aria-label={label}
      title={label}
      onClick={onToggle}
      className={cn(cls, 'cursor-pointer text-left hover:bg-hover hover:text-soft')}
    >
      {content}
    </button>
  ) : (
    <div className={cls} title={label}>
      {content}
    </div>
  )
}

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`

function CreateForm({
  hostId,
  worktree,
  branch,
  defaultBranch,
  disabled,
  creating,
  onDone
}: {
  hostId: string
  worktree: string
  branch: GitBranchInfo
  defaultBranch: string | null
  disabled: boolean
  creating: boolean
  onDone: () => void
}) {
  const [title, setTitle] = useState('')
  const [body, setBody] = useState('')
  const [base, setBase] = useState(defaultBranch ?? '')
  const [draft, setDraft] = useState(false)
  const push = needsPush(branch)
  const submit = async () => {
    if (disabled) return
    const pr = await createPullRequest(hostId, worktree, branch, {
      title,
      body,
      base: base.trim() || null,
      draft
    })
    if (pr) onDone()
  }
  const field =
    'w-full rounded-[5px] border border-border bg-transparent px-2 py-1 text-sm text-soft outline-none placeholder:text-dim focus-visible:border-ring disabled:opacity-50'
  return (
    <form
      className="flex flex-col gap-1.5 px-3 pt-1 pb-2.5"
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && !creating) onDone()
        if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
          e.preventDefault()
          void submit()
        }
      }}
    >
      <input
        aria-label="Pull request title"
        autoFocus
        value={title}
        disabled={creating}
        onChange={(e) => setTitle(e.target.value)}
        placeholder="Title (empty: from the commits)"
        className={field}
      />
      <textarea
        aria-label="Pull request description"
        value={body}
        disabled={creating}
        onChange={(e) => setBody(e.target.value)}
        rows={3}
        placeholder="Description (Markdown)"
        className={cn(field, 'field-sizing-content max-h-48 min-h-[60px] resize-none py-1.5')}
      />
      <div className="flex min-w-0 items-center gap-2 text-xs text-quiet">
        <label className="flex min-w-0 flex-1 items-center gap-1.5">
          <span className="shrink-0">Into</span>
          <input
            aria-label="Base branch"
            value={base}
            disabled={creating}
            onChange={(e) => setBase(e.target.value)}
            placeholder="default branch"
            className={cn(field, 'min-w-0 py-0.5 font-mono text-xs')}
          />
        </label>
        <label className="flex shrink-0 cursor-pointer items-center gap-1.5 select-none">
          <Checkbox
            checked={draft}
            disabled={creating}
            onCheckedChange={(v) => setDraft(v === true)}
            className="size-3.5"
          />
          Draft
        </label>
      </div>
      <div className="flex items-center justify-end gap-2">
        {creating ? (
          <span className="mr-auto flex items-center gap-1.5 text-xs text-quiet">
            <Spinner className="size-3" />
            Opening…
          </span>
        ) : (
          push && (
            <span className="mr-auto min-w-0 truncate text-xs text-faint">
              {branch.upstream ? `${branch.ahead} unpushed` : 'Not on the remote'}
            </span>
          )
        )}
        <Button size="xs" variant="ghost" type="button" disabled={creating} onClick={onDone}>
          Cancel
        </Button>
        <Button size="xs" type="submit" disabled={disabled}>
          <GitPullRequestCreate />
          {push
            ? branch.upstream
              ? 'Push & create'
              : 'Publish & create'
            : draft
              ? 'Create draft'
              : 'Create'}
        </Button>
      </div>
    </form>
  )
}
