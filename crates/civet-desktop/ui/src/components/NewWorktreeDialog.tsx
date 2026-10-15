// "New worktree + launch agent". Ported in spirit from Orca's
// src/renderer/src/components/NewWorkspaceComposerCard.tsx /
// NewWorkspaceComposerModal.tsx (name, base branch, agent picker); the
// composer prompt, issue linking, setup scripts and file drop were dropped.
// Calls `launch_agent`, which creates the worktree and spawns the agent pane.

import { useMemo, useState } from 'react'
import { Loader2, Sparkles } from 'lucide-react'
import { launchAgentInNewWorktree, useStore, type AgentChoice } from '../state/store'
import { splitShellWords } from '../lib/shell-words'
import { cn } from '../lib/utils'
import type { CommandSpec } from '../types'
import { Button, Dialog, Input, Label } from './ui'

const AGENTS: { id: AgentChoice; label: string; hint: string }[] = [
  { id: 'claude', label: 'Claude Code', hint: 'claude' },
  { id: 'codex', label: 'Codex', hint: 'codex' },
  { id: 'custom', label: 'Custom command', hint: 'any CLI' }
]

/** Git-ish branch name check (subset of git check-ref-format). */
export function branchNameError(name: string): string | null {
  if (!name.trim()) return 'Name is required'
  if (/\s/.test(name)) return 'No spaces'
  if (/(^[/.-])|([/.]$)|\.\.|[~^:?*[\\]|@\{|\.lock$/.test(name)) return 'Not a valid branch name'
  return null
}

export function NewWorktreeDialog() {
  const req = useStore((s) => s.newWorktree)
  const repos = useStore((s) => (req ? s.repos[req.hostId] : undefined))
  const [repoId, setRepoId] = useState(req?.repo ?? repos?.[0]?.id ?? '')
  const repo = repos?.find((r) => r.id === repoId)
  const [name, setName] = useState('')
  const [base, setBase] = useState<string | null>(null)
  const [agent, setAgent] = useState<AgentChoice>('claude')
  const [custom, setCustom] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const command = useMemo((): { spec: CommandSpec | null; error: string | null } => {
    if (agent === 'claude') return { spec: { program: 'claude', args: [] }, error: null }
    if (agent === 'codex') return { spec: { program: 'codex', args: [] }, error: null }
    try {
      const words = splitShellWords(custom)
      if (words.length === 0) return { spec: null, error: 'Enter a command' }
      return { spec: { program: words[0]!, args: words.slice(1) }, error: null }
    } catch (e) {
      return { spec: null, error: (e as Error).message }
    }
  }, [agent, custom])

  if (!req) return null
  const close = () => useStore.setState({ newWorktree: null })
  const nameError = name ? branchNameError(name) : null
  const baseValue = base ?? repo?.defaultBranch ?? ''
  const canSubmit = !busy && repo && !branchNameError(name) && command.spec

  async function submit() {
    if (!req || !repo || !command.spec) return
    setBusy(true)
    setError(null)
    try {
      await launchAgentInNewWorktree({
        hostId: req.hostId,
        repo: repo.id,
        branch: name.trim(),
        base: baseValue.trim() || null,
        agent,
        command: command.spec,
        title: name.trim()
      })
      close()
    } catch (err) {
      setError((err as Error).message ?? String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog
      title="New worktree + launch agent"
      description="Creates a git worktree on the host and starts the agent in it."
      onClose={close}
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault()
          void submit()
        }}
      >
        <div className="grid grid-cols-2 gap-3">
          <div className="col-span-2 flex flex-col gap-1.5">
            <Label htmlFor="nw-name">Name (branch)</Label>
            <Input
              id="nw-name"
              placeholder="fix-reconnect-banner"
              value={name}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={nameError ? true : undefined}
              onChange={(e) => setName(e.target.value)}
            />
            {nameError && <span className="text-[11px] text-destructive">{nameError}</span>}
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="nw-repo">Repository</Label>
            <select
              id="nw-repo"
              className="h-9 rounded-md border border-input bg-transparent px-2 text-sm dark:bg-input/30"
              value={repoId}
              onChange={(e) => {
                setRepoId(e.target.value)
                setBase(null)
              }}
            >
              {(repos ?? []).map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
            </select>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="nw-base">Base branch</Label>
            <Input
              id="nw-base"
              placeholder={repo?.defaultBranch ?? 'HEAD'}
              value={baseValue}
              spellCheck={false}
              onChange={(e) => setBase(e.target.value)}
            />
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label>Agent</Label>
          <div className="grid grid-cols-3 gap-2" role="radiogroup" aria-label="Agent">
            {AGENTS.map((a) => (
              <button
                key={a.id}
                type="button"
                role="radio"
                aria-checked={agent === a.id}
                onClick={() => setAgent(a.id)}
                className={cn(
                  'flex flex-col items-start rounded-md border px-3 py-2 text-left text-sm transition-colors',
                  agent === a.id
                    ? 'border-ring bg-accent text-accent-foreground'
                    : 'border-border text-muted-foreground hover:bg-accent/50'
                )}
              >
                <span className="font-medium text-foreground">{a.label}</span>
                <span className="font-mono text-[11px]">{a.hint}</span>
              </button>
            ))}
          </div>
          {agent === 'custom' && (
            <>
              <Input
                aria-label="Custom command"
                className="font-mono"
                placeholder={`aider --model sonnet`}
                value={custom}
                spellCheck={false}
                onChange={(e) => setCustom(e.target.value)}
              />
              {custom && command.error && (
                <span className="text-[11px] text-destructive">{command.error}</span>
              )}
            </>
          )}
        </div>
        {error && <p className="text-xs text-destructive">{error}</p>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button type="submit" disabled={!canSubmit}>
            {busy ? <Loader2 className="animate-spin" /> : <Sparkles />}
            Create and launch
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
