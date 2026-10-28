// "Send prompt" dialog: one prompt to several agents (broadcast), now or
// queued until each agent is done (the daemon sends it then, even with this
// app closed). Targets are every live agent pane, grouped by worktree with
// checkboxes; the sidebar selection or the row it was opened from comes
// checked. Also edits or cancels a queued prompt (click its sidebar row).
// ⇧⌘B opens it; ⌘↵ sends.
import { useMemo, useState } from 'react'
import { allWorktrees, useStore, wsKey } from '@/state/store'
import {
  broadcastPrompt,
  cancelQueuedPrompt,
  closePromptDialog,
  editQueuedPrompt,
  hasPrompts,
  openPromptDialog,
  requeuePrompt,
  usePrompts,
  type PromptDialog as DialogRequest
} from '@/state/prompts'
import {
  paneTargetLabel,
  refKey,
  targetGroups,
  WHEN_LABEL,
  type PaneRef,
  type PromptWhen
} from '@/lib/prompts'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { paneDot } from '@/lib/worktree-view'
import { cn } from '@/lib/utils'
import type { PromptTrigger } from '@/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Spinner } from '@/components/ui/spinner'
import { Textarea } from '@/components/ui/textarea'
import { Kbd } from './chrome'
import { StatusDot } from './StatusDot'

const FIELD =
  'border-border bg-field shadow-none dark:bg-field text-foreground placeholder:text-faint focus-visible:ring-0 focus-visible:border-border-strong'

const MOD = typeof navigator !== 'undefined' && navigator.platform.startsWith('Mac') ? '⌘' : 'Ctrl+'

export function PromptDialog() {
  const req = usePrompts((s) => s.dialog)
  return (
    <Dialog open={req !== null} onOpenChange={(open) => !open && closePromptDialog()}>
      <DialogContent
        showCloseButton={false}
        className="w-[580px] max-w-[calc(100vw-32px)] sm:max-w-[580px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        {req?.kind === 'send' && <SendForm key="send" req={req} />}
        {req?.kind === 'edit' && <EditForm key={req.prompt.id} req={req} />}
      </DialogContent>
    </Dialog>
  )
}

function WhenPicker({
  value,
  options,
  onChange
}: {
  value: PromptWhen
  options: readonly PromptWhen[]
  onChange: (w: PromptWhen) => void
}) {
  return (
    <div className="flex flex-wrap gap-1" role="radiogroup" aria-label="When">
      {options.map((w) => (
        <button
          key={w}
          type="button"
          role="radio"
          aria-checked={w === value}
          onClick={() => onChange(w)}
          className={cn(
            'h-7 cursor-pointer rounded-[5px] border px-2.5 text-sm',
            w === value
              ? 'border-border-strong bg-white/[0.08] text-foreground'
              : 'border-white/[0.08] bg-transparent text-muted-foreground hover:text-foreground'
          )}
        >
          {WHEN_LABEL[w]}
        </button>
      ))}
    </div>
  )
}

function PromptText({
  value,
  onChange,
  onSubmit,
  placeholder
}: {
  value: string
  onChange: (v: string) => void
  onSubmit: () => void
  placeholder: string
}) {
  return (
    <Textarea
      autoFocus
      aria-label="Prompt"
      value={value}
      rows={4}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
          e.preventDefault()
          onSubmit()
        }
      }}
      className={cn(
        'field-sizing-fixed max-h-[40vh] min-h-0 resize-none rounded-[5px] px-2.5 py-2 text-sm leading-normal',
        FIELD
      )}
    />
  )
}

function SendForm({ req }: { req: Extract<DialogRequest, { kind: 'send' }> }) {
  const hosts = useStore((s) => s.hosts)
  const repos = useStore((s) => s.repos)
  const worktrees = useStore((s) => s.worktrees)
  const panes = useStore((s) => s.panes)
  const [text, setText] = useState('')
  const [when, setWhen] = useState<PromptWhen>(req.when ?? 'now')
  const [checked, setChecked] = useState(() => new Set(req.targets.map(refKey)))
  const [busy, setBusy] = useState(false)

  const groups = useMemo(() => {
    void worktrees // recompute when worktrees change (allWorktrees reads the store)
    const list = allWorktrees()
      .filter(({ hostId }) => hasPrompts(hosts[hostId]))
      .map(({ hostId, wt }) => ({
        hostId,
        wt,
        project: repos[hostId]?.find((r) => r.id === wt.repo)?.name ?? wt.repo,
        host: hosts[hostId]?.label ?? hostId
      }))
    return targetGroups(list, panes, new Set(list.map((x) => x.hostId)).size > 1)
  }, [hosts, repos, worktrees, panes])

  const all: PaneRef[] = groups.flatMap((g) =>
    g.panes.map((p) => ({ hostId: g.hostId, pane: p.id }))
  )
  const targets = all.filter((r) => checked.has(refKey(r)))
  const canSubmit = !busy && !!text.trim() && targets.length > 0
  const toggle = (refs: PaneRef[], on: boolean) =>
    setChecked((cur) => {
      const next = new Set(cur)
      for (const r of refs) on ? next.add(refKey(r)) : next.delete(refKey(r))
      return next
    })

  async function submit() {
    if (!canSubmit) return
    setBusy(true)
    const outcome = await broadcastPrompt(targets, text, when)
    setBusy(false)
    if (outcome.ok > 0) closePromptDialog()
  }

  const n = targets.length
  const agents = `${n} agent${n === 1 ? '' : 's'}`
  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      <DialogTitle>Send prompt</DialogTitle>
      <DialogDescription className="-mt-1.5 text-xs text-muted-foreground">
        Typed into each agent and submitted. Queued prompts wait on the host until that agent's turn
        ends, then go out one per turn.
      </DialogDescription>
      <PromptText
        value={text}
        onChange={setText}
        onSubmit={() => void submit()}
        placeholder="Prompt for the selected agents"
      />
      <WhenPicker value={when} options={['now', 'done', 'doneOrNeedsInput']} onChange={setWhen} />
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="flex-1">
          {all.length ? `${n} of ${all.length} agents` : 'No running agents on connected hosts'}
        </span>
        {all.length > 0 && (
          <>
            <button
              type="button"
              className="cursor-pointer hover:text-foreground"
              onClick={() => toggle(all, true)}
            >
              All
            </button>
            <span className="text-dim">·</span>
            <button
              type="button"
              className="cursor-pointer hover:text-foreground"
              onClick={() => toggle(all, false)}
            >
              None
            </button>
          </>
        )}
      </div>
      {groups.length > 0 && (
        <div
          className="max-h-[min(300px,36vh)] overflow-y-auto rounded-[5px] border border-border py-1"
          aria-label="Agents"
        >
          {groups.map((g) => {
            const refs = g.panes.map((p) => ({ hostId: g.hostId, pane: p.id }))
            const on = refs.filter((r) => checked.has(refKey(r))).length
            const connected = hosts[g.hostId]?.state === 'connected'
            return (
              <div key={wsKey(g.hostId, g.worktree.id)} data-target-wt={g.worktree.id}>
                <label className="flex cursor-pointer items-center gap-2.5 px-2.5 py-1 text-xs font-medium text-muted-foreground hover:bg-hover">
                  <Checkbox
                    checked={on === refs.length ? true : on ? 'indeterminate' : false}
                    onCheckedChange={(v) => toggle(refs, v === true)}
                    className="size-[13px] rounded-[3px] [&_svg]:size-2.5"
                  />
                  <span className="min-w-0 truncate">{g.label}</span>
                </label>
                {g.panes.map((p) => {
                  const ref = { hostId: g.hostId, pane: p.id }
                  return (
                    <label
                      key={p.id}
                      data-target-pane={p.id}
                      className="flex cursor-pointer items-center gap-2.5 py-1 pr-2.5 pl-7 text-sm hover:bg-hover"
                    >
                      <Checkbox
                        checked={checked.has(refKey(ref))}
                        onCheckedChange={(v) => toggle([ref], v === true)}
                        className="size-[13px] rounded-[3px] [&_svg]:size-2.5"
                      />
                      <StatusDot kind={paneDot(p, connected)} size={6} showIdle />
                      <span className="min-w-0 flex-1 truncate text-body">
                        {paneTargetLabel(p)}
                      </span>
                    </label>
                  )
                })}
              </div>
            )
          })}
        </div>
      )}
      <div className="flex items-center justify-end gap-2">
        <span className="mr-auto text-2xs text-faint">
          <Kbd>{MOD}↵</Kbd> to {when === 'now' ? 'send' : 'queue'}
        </span>
        <Button type="button" variant="outline" size="md" onClick={closePromptDialog}>
          Cancel
        </Button>
        <Button type="submit" size="md" disabled={!canSubmit}>
          {busy && <Spinner className="size-3.5" />}
          {when === 'now' ? `Send to ${agents}` : `Queue for ${agents}`}
        </Button>
      </div>
    </form>
  )
}

function EditForm({ req }: { req: Extract<DialogRequest, { kind: 'edit' }> }) {
  const pane = useStore((s) => s.panes[req.hostId]?.[req.prompt.pane])
  const connected = useStore((s) => s.hosts[req.hostId]?.state === 'connected')
  const [text, setText] = useState(req.prompt.text)
  const [when, setWhen] = useState<PromptWhen>(req.prompt.trigger)
  const [busy, setBusy] = useState(false)
  const canSubmit = !busy && !!text.trim()
  const target = [{ hostId: req.hostId, pane: req.prompt.pane }]

  async function save() {
    if (!canSubmit) return
    setBusy(true)
    try {
      if (when === 'now') {
        // Off the queue first, so it can never go out twice; if that fails
        // the daemon has (most likely) just sent it.
        if (!(await cancelQueuedPrompt(req.hostId, req.prompt))) return
        if (!(await broadcastPrompt(target, text, 'now')).ok) {
          // Not sent: back in the queue (at its end), and the dialog edits
          // that one, so Close or Cancel prompt behave as before.
          const back = await requeuePrompt(req.hostId, req.prompt.pane, text, req.prompt.trigger)
          if (back) openPromptDialog({ kind: 'edit', hostId: req.hostId, prompt: back })
          return
        }
      } else {
        await editQueuedPrompt(req.hostId, req.prompt.id, text, when as PromptTrigger)
      }
      closePromptDialog()
    } catch {
      // Reported by the action.
    } finally {
      setBusy(false)
    }
  }

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      <DialogTitle>Queued prompt</DialogTitle>
      <DialogDescription className="-mt-1.5 flex items-center gap-2 text-xs text-muted-foreground">
        {pane && <StatusDot kind={paneDot(pane, connected)} size={6} showIdle />}
        <span className="truncate">{pane ? paneTargetLabel(pane) : `Pane ${req.prompt.pane}`}</span>
      </DialogDescription>
      <PromptText
        value={text}
        onChange={setText}
        onSubmit={() => void save()}
        placeholder="Prompt"
      />
      <WhenPicker value={when} options={['done', 'doneOrNeedsInput', 'now']} onChange={setWhen} />
      <div className="flex items-center justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          size="md"
          className="mr-auto text-status-err-text"
          disabled={busy}
          onClick={() => {
            void cancelQueuedPrompt(req.hostId, req.prompt)
            closePromptDialog()
          }}
        >
          Cancel prompt
        </Button>
        <Button type="button" variant="outline" size="md" onClick={closePromptDialog}>
          Close
        </Button>
        <Button type="submit" size="md" disabled={!canSubmit}>
          {busy && <Spinner className="size-3.5" />}
          {when === 'now' ? 'Send now' : 'Save'}
        </Button>
      </div>
    </form>
  )
}
