// "New worktree" (Civet v3): project chips, then the task for the first agent,
// which names the branch (a slug like `civet handoff`'s, suffixed past the
// project's taken names), a preview line where the branch and base can be
// edited (no task and no branch: the steps run in the project's main
// checkout, which the line under it says), ordered on-create steps (Claude / Codex / Terminal,
// each opens in its own tab) and "Save as default for <project>" (off: one
// worktree's steps don't change the next one's unless asked). Enter starts it. The first agent step goes through `launch_agent`, so the
// single-call "worktree + agent" path is unchanged; see
// store.createWorktreeWithSteps. With a worktree template (Settings →
// Projects or `.civet/worktree.toml`) a Setup row runs it first, and its
// agent command and prompt are the defaults (steps saved here still win).

import { useEffect, useState } from 'react'
import { createWorktreeWithSteps, isFolderRepo, useStore, wtKey } from '@/state/store'
import {
  agentCommandFor,
  setPrefs,
  stepAgent,
  usePrefs,
  type CreateStep,
  type StepKind
} from '@/state/prefs'
import { splitShellWords } from '@/lib/shell-words'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { branchSlug, freeBranch } from '@/lib/branch-name'
import { worktreeBranch } from '@/lib/worktree-view'
import {
  agentStepKind,
  describeTemplate,
  hasWorktreeTemplates,
  loadTemplate,
  useTemplates
} from '@/state/templates'
import type { WorktreeTemplate } from '@/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Spinner } from '@/components/ui/spinner'
import { ProjectTile } from './chrome'

/** Git-ish branch name check (subset of git check-ref-format). */
export function branchNameError(name: string): string | null {
  if (!name.trim()) return 'Name is required'
  if (/\s/.test(name)) return 'No spaces'
  if (/(^[/.-])|([/.]$)|\.\.|[~^:?*[\\]|@\{|\.lock$/.test(name)) return 'Not a valid branch name'
  return null
}

const FIELD =
  'border-border bg-field shadow-none dark:bg-field font-mono text-foreground placeholder:text-faint focus-visible:ring-0 focus-visible:border-border-strong'

const STEP_KINDS: readonly [StepKind, string][] = [
  ['agent', 'Claude'],
  ['codex', 'Codex'],
  ['terminal', 'Terminal']
]

/** A step's command after switching its kind: an agent's default command follows the kind. */
function switchedCmd(from: CreateStep, to: StepKind): string {
  const prev = stepAgent(from.kind)
  const next = stepAgent(to)
  const isDefault = !from.cmd.trim() || (prev !== null && from.cmd === agentCommandFor(prev))
  if (!isDefault) return from.cmd
  return next ? agentCommandFor(next) : ''
}

/** Steps saved for the project here, else one agent: the template's command, else Settings'. */
function defaultSteps(
  hostId: string,
  repo: string,
  template?: WorktreeTemplate | null
): CreateStep[] {
  const saved = usePrefs.getState().createSteps[wtKey(hostId, repo)]
  if (saved) return saved.map((s) => ({ ...s }))
  const agent = template?.agent?.trim()
  if (agent) return [{ kind: agentStepKind(agent), cmd: agent }]
  return [{ kind: 'agent', cmd: usePrefs.getState().agentCommand }]
}

export function NewWorktreeDialog() {
  const req = useStore((s) => s.newWorktree)
  const allRepos = useStore((s) => (req ? s.repos[req.hostId] : undefined))
  // Folder projects have no worktrees (filtered here: a new array in the
  // selector would re-render forever).
  const repos = allRepos?.filter((r) => !isFolderRepo(r))
  const hostLabel = useStore((s) => (req ? (s.hosts[req.hostId]?.label ?? req.hostId) : ''))
  const [repoId, setRepoId] = useState(
    (repos?.some((r) => r.id === req?.repo) ? req?.repo : undefined) ?? repos?.[0]?.id ?? ''
  )
  const repo = repos?.find((r) => r.id === repoId)
  // null: the branch follows the task; a string: the one typed in the preview line.
  const [branchEdit, setBranchEdit] = useState<string | null>(null)
  const [base, setBase] = useState<string | null>(null)
  const [steps, setSteps] = useState<CreateStep[]>(() =>
    req ? defaultSteps(req.hostId, repoId) : []
  )
  const [save, setSave] = useState(false)
  const [prompt, setPrompt] = useState('')
  // Template defaults arrive after the dialog opens: they replace the steps
  // and prompt only while those are untouched.
  const [touched, setTouched] = useState({ steps: false, prompt: false })
  const [runSetup, setRunSetup] = useState(true)
  const templates = req ? hasWorktreeTemplates(req.hostId) : false
  const template = useTemplates((s) => (req ? s[wtKey(req.hostId, repoId)]?.info?.effective : null))
  useEffect(() => {
    if (req && repoId && templates) void loadTemplate(req.hostId, repoId)
  }, [req, repoId, templates])
  // biome-ignore lint/correctness/useExhaustiveDependencies: only when the template arrives
  useEffect(() => {
    if (!req || !template) return
    if (!touched.steps) setSteps(defaultSteps(req.hostId, repoId, template))
    if (!touched.prompt) setPrompt(template.prompt ?? '')
  }, [template])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const worktrees = useStore((s) => (req ? s.worktrees[wtKey(req.hostId, repoId)] : undefined))
  const mainWt = worktrees?.find((w) => w.isMain)

  if (!req) return null
  const close = () => useStore.setState({ newWorktree: null })
  // A template's prompt prefills the task; only what is typed after it names the branch.
  const templatePrompt = template?.prompt?.trim() ?? ''
  const brief =
    templatePrompt && prompt.trimStart().startsWith(templatePrompt)
      ? prompt.trimStart().slice(templatePrompt.length)
      : prompt
  const stem = branchSlug(brief)
  const taken = new Set((worktrees ?? []).flatMap((w) => (w.branch ? [w.branch] : [])))
  const auto = branchEdit === null
  const name = auto ? (stem ? freeBranch(stem, taken) : '') : branchEdit
  const nameError = name ? branchNameError(name) : null
  const useMain = !name.trim()
  const baseValue = base ?? repo?.defaultBranch ?? ''
  const stepError = steps
    .map((st, i) => {
      if (!stepAgent(st.kind) || !st.cmd.trim()) return null
      try {
        splitShellWords(st.cmd)
        return null
      } catch (e) {
        return `Step ${i + 1}: ${(e as Error).message}`
      }
    })
    .find(Boolean)
  const canSubmit = !busy && !!repo && !nameError && !stepError
  const firstAgent = steps.map((s) => stepAgent(s.kind)).find((a) => a !== null)
  const hasAgent = !!firstAgent
  const agentLabel = firstAgent === 'codex' ? 'Codex' : 'Claude'
  const editSteps = (f: (list: CreateStep[]) => CreateStep[]) => {
    setTouched((t) => ({ ...t, steps: true }))
    setSteps(f)
  }
  const update = (i: number, patch: Partial<CreateStep>) =>
    editSteps((list) => list.map((s, j) => (j === i ? { ...s, ...patch } : s)))
  const setupSummary = template ? describeTemplate(template) : ''
  const willSetup = !useMain && runSetup && !!setupSummary

  async function submit() {
    if (!req || !repo || !canSubmit) return
    setBusy(true)
    setError(null)
    try {
      if (save) {
        setPrefs({
          createSteps: {
            ...usePrefs.getState().createSteps,
            [wtKey(req.hostId, repo.id)]: steps.map((s) => ({ ...s }))
          }
        })
      }
      await createWorktreeWithSteps({
        hostId: req.hostId,
        repo: repo.id,
        branch: name.trim(),
        base: baseValue.trim() || null,
        steps,
        prompt,
        setup: willSetup,
        autoBranch: auto
      })
      close()
    } catch (err) {
      setError((err as Error).message ?? String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && close()}>
      <DialogContent
        showCloseButton={false}
        className="w-[540px] max-w-[calc(100vw-32px)] sm:max-w-[540px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <DialogTitle>New worktree</DialogTitle>
        <DialogDescription className="sr-only">
          Creates a git worktree on {hostLabel} and runs the on-create steps in it.
        </DialogDescription>
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault()
            void submit()
          }}
        >
          <div className="flex flex-wrap gap-1" role="radiogroup" aria-label="Project">
            {(repos ?? []).map((r) => (
              <button
                key={r.id}
                type="button"
                role="radio"
                aria-checked={r.id === repoId}
                onClick={() => {
                  setRepoId(r.id)
                  setBase(null)
                  setBranchEdit(null)
                  const t = useTemplates.getState()[wtKey(req.hostId, r.id)]?.info?.effective
                  setSteps(defaultSteps(req.hostId, r.id, t))
                  setPrompt(t?.prompt ?? '')
                  setTouched({ steps: false, prompt: false })
                }}
                className={cn(
                  'flex h-7 cursor-pointer items-center gap-[7px] rounded-[5px] border px-2.5 text-sm',
                  r.id === repoId
                    ? 'border-border-strong bg-white/[0.08]'
                    : 'border-white/[0.08] bg-transparent'
                )}
              >
                <ProjectTile name={r.name} size={12} />
                {r.name}
              </button>
            ))}
          </div>
          <Textarea
            aria-label={hasAgent ? `Task for ${agentLabel}` : 'Task'}
            autoFocus
            value={prompt}
            rows={3}
            placeholder={
              hasAgent
                ? `What should ${agentLabel} do? Names the branch, too`
                : 'What is it for? Names the branch'
            }
            onChange={(e) => {
              setTouched((t) => ({ ...t, prompt: true }))
              setPrompt(e.target.value)
            }}
            onFocus={(e) => {
              // A template's prefilled prompt: type after it, not before.
              const end = e.currentTarget.value.length
              e.currentTarget.setSelectionRange(end, end)
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault()
                void submit()
              }
            }}
            className={cn(
              'field-sizing-fixed min-h-0 resize-none rounded-[5px] px-2.5 py-2 font-sans text-sm leading-normal',
              FIELD,
              'font-sans'
            )}
          />
          <div className="flex flex-col gap-1">
            <div className="flex min-w-0 items-center font-mono text-2xs text-faint">
              <span className="shrink-0">
                {hostLabel}:{repo?.name ?? '…'}/
              </span>
              <input
                aria-label="Branch"
                title={auto ? 'Branch, named from the task; type to change it' : 'Branch'}
                value={name}
                placeholder="branch"
                autoComplete="off"
                spellCheck={false}
                aria-invalid={nameError ? true : undefined}
                onChange={(e) => setBranchEdit(e.target.value)}
                style={{ width: `${Math.max(6, name.length + 1)}ch` }}
                className="min-w-0 shrink rounded-sm border-none bg-transparent p-0 font-mono text-2xs text-muted-foreground underline decoration-dim decoration-dotted underline-offset-2 outline-none placeholder:text-dim hover:text-foreground focus:text-foreground"
              />
              {!auto && stem && (
                <button
                  type="button"
                  title="Name the branch from the task again"
                  aria-label="Name the branch from the task"
                  onClick={() => setBranchEdit(null)}
                  className="ml-1 shrink-0 cursor-pointer rounded-sm px-0.5 text-dim hover:text-foreground"
                >
                  ↺
                </button>
              )}
              {!useMain && (
                <>
                  <span className="shrink-0">&nbsp;· from&nbsp;</span>
                  <input
                    aria-label="Base branch"
                    title="Base branch"
                    value={baseValue}
                    placeholder="HEAD"
                    spellCheck={false}
                    onChange={(e) => setBase(e.target.value)}
                    style={{ width: `${Math.max(4, baseValue.length + 1)}ch` }}
                    className="shrink-0 rounded-sm border-none bg-transparent p-0 font-mono text-2xs text-muted-foreground underline decoration-dim decoration-dotted underline-offset-2 outline-none hover:text-foreground focus:text-foreground"
                  />
                </>
              )}
            </div>
            {nameError && <span className="text-2xs text-status-err-text">{nameError}</span>}
            {useMain && (
              <div className="truncate text-2xs text-status-wait">
                No branch: runs in {repo?.name ?? '…'}'s main checkout
                {mainWt ? ` (${worktreeBranch(mainWt)})` : ''}, no new worktree
              </div>
            )}
          </div>
          <div className="flex flex-col gap-1.5">
            <div className="flex items-baseline justify-between text-xs">
              <span className="font-medium text-body">On create</span>
              <span className="text-xs text-faint">
                {steps.length ? 'runs in order, each in its own tab' : 'just creates the worktree'}
              </span>
            </div>
            <div className="overflow-hidden rounded-lg border border-border">
              {!useMain && setupSummary && (
                <label
                  className="flex cursor-pointer items-center gap-2 border-b border-white/[0.08] px-2 py-1.5"
                  title="From the project's worktree template (Settings → Projects, .civet/worktree.toml)"
                >
                  <Checkbox
                    aria-label="Run worktree setup"
                    checked={runSetup}
                    onCheckedChange={(v) => setRunSetup(v === true)}
                    className="size-[13px] rounded-[3px] [&_svg]:size-2.5"
                  />
                  <span className="shrink-0 text-xs text-body">Setup</span>
                  <span className="min-w-0 flex-1 truncate font-mono text-2xs text-muted-foreground">
                    {setupSummary}
                  </span>
                </label>
              )}
              {steps.map((st, i) => (
                <div
                  key={i}
                  className="flex items-center gap-2 border-b border-white/[0.08] px-2 py-1.5"
                >
                  <span className="w-3 text-right font-mono text-2xs text-dim">{i + 1}</span>
                  <div
                    className="flex shrink-0 gap-0.5 rounded-md bg-muted p-0.5"
                    role="radiogroup"
                    aria-label={`Step ${i + 1} kind`}
                  >
                    {STEP_KINDS.map(([k, label]) => (
                      <button
                        key={k}
                        type="button"
                        role="radio"
                        aria-checked={st.kind === k}
                        onClick={() => update(i, { kind: k, cmd: switchedCmd(st, k) })}
                        className={cn(
                          'h-5 cursor-pointer rounded-sm px-2 text-xs',
                          st.kind === k
                            ? 'bg-field text-foreground'
                            : 'bg-transparent text-muted-foreground'
                        )}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  <Input
                    aria-label={`Step ${i + 1} command`}
                    value={st.cmd}
                    placeholder={
                      st.kind === 'agent'
                        ? 'claude'
                        : st.kind === 'codex'
                          ? 'codex'
                          : 'pnpm install'
                    }
                    spellCheck={false}
                    onChange={(e) => update(i, { cmd: e.target.value })}
                    className={cn(
                      'h-[26px] min-w-0 flex-1 rounded-[5px] border-white/[0.08] px-2 text-xs',
                      FIELD
                    )}
                  />
                  <button
                    type="button"
                    title="Remove step"
                    aria-label={`Remove step ${i + 1}`}
                    onClick={() => editSteps((list) => list.filter((_, j) => j !== i))}
                    className="size-[22px] cursor-pointer rounded-sm p-0 text-sm text-faint hover:bg-muted hover:text-foreground"
                  >
                    ×
                  </button>
                </div>
              ))}
              <div className="flex items-center gap-2.5 px-2 py-1.5">
                <button
                  type="button"
                  onClick={() => editSteps((list) => [...list, { kind: 'terminal', cmd: '' }])}
                  className="h-6 cursor-pointer rounded-[5px] border border-dashed border-white/[0.16] px-[9px] text-xs text-muted-foreground hover:border-white/30 hover:text-foreground"
                >
                  + Add step
                </button>
                <span className="flex-1" />
                <label className="flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
                  <Checkbox
                    checked={save}
                    onCheckedChange={(v) => setSave(v === true)}
                    className="size-[13px] rounded-[3px] [&_svg]:size-2.5"
                  />
                  Save as default for {repo?.name ?? 'this project'}
                </label>
              </div>
            </div>
            {stepError && <span className="text-2xs text-status-err-text">{stepError}</span>}
          </div>
          {error && <p className="text-xs text-status-err-text">{error}</p>}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" size="md" onClick={close} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" size="md" disabled={!canSubmit}>
              {busy && <Spinner className="size-3.5" />}
              {useMain ? 'Open in main checkout' : 'Create'}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
