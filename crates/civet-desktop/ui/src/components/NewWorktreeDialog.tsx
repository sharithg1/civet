// "New worktree + launch agent". Ported in spirit from Orca's
// src/renderer/src/components/NewWorkspaceComposerCard.tsx /
// NewWorkspaceComposerModal.tsx (name, base branch, agent picker); the
// composer prompt, issue linking, setup scripts and file drop were dropped.
// Calls `launch_agent`, which creates the worktree and spawns the agent pane.

import { useMemo, useState } from 'react'
import { Sparkles } from 'lucide-react'
import { launchAgentInNewWorktree, useStore, type AgentChoice } from '@/state/store'
import { splitShellWords } from '@/lib/shell-words'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import type { CommandSpec } from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'

const AGENTS: { id: AgentChoice; label: string; hint: string }[] = [
  { id: 'claude', label: 'Claude Code', hint: 'claude' },
  { id: 'codex', label: 'Codex', hint: 'codex' },
  { id: 'custom', label: 'Custom command', hint: 'any CLI' }
]

const LABEL = 'text-xs text-muted-foreground'

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
    <Dialog open onOpenChange={(open) => !open && close()}>
      <DialogContent onCloseAutoFocus={restoreTerminalFocus}>
        <DialogHeader className="gap-1.5">
          <DialogTitle className="text-base leading-none">New worktree + launch agent</DialogTitle>
          <DialogDescription>
            Creates a git worktree on the host and starts the agent in it.
          </DialogDescription>
        </DialogHeader>
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault()
            void submit()
          }}
        >
          <div className="grid grid-cols-2 gap-3">
            <div className="col-span-2 flex flex-col gap-1.5">
              <Label className={LABEL} htmlFor="nw-name">
                Name (branch)
              </Label>
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
              <Label className={LABEL} htmlFor="nw-repo">
                Repository
              </Label>
              <Select
                value={repoId}
                onValueChange={(value) => {
                  setRepoId(value)
                  setBase(null)
                }}
              >
                <SelectTrigger id="nw-repo" className="w-full">
                  <SelectValue placeholder="No repositories" />
                </SelectTrigger>
                <SelectContent>
                  {(repos ?? []).map((r) => (
                    <SelectItem key={r.id} value={r.id}>
                      {r.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label className={LABEL} htmlFor="nw-base">
                Base branch
              </Label>
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
            <Label className={LABEL} id="nw-agent-label">
              Agent
            </Label>
            <ToggleGroup
              type="single"
              value={agent}
              // Single toggle groups can be emptied by re-clicking; keep a choice.
              onValueChange={(value) => value && setAgent(value as AgentChoice)}
              aria-labelledby="nw-agent-label"
              className="grid w-full grid-cols-3 gap-2"
            >
              {AGENTS.map((a) => (
                <ToggleGroupItem
                  key={a.id}
                  value={a.id}
                  className={cn(
                    'flex h-auto flex-col items-start gap-0 rounded-md border px-3 py-2 text-left text-sm font-normal whitespace-normal transition-colors',
                    'border-border text-muted-foreground hover:bg-accent/50 hover:text-muted-foreground',
                    'data-[state=on]:border-ring data-[state=on]:bg-accent data-[state=on]:text-accent-foreground'
                  )}
                >
                  <span className="font-medium text-foreground">{a.label}</span>
                  <span className="font-mono text-[11px]">{a.hint}</span>
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
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
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={close} disabled={busy}>
              Cancel
            </Button>
            <Button type="submit" disabled={!canSubmit}>
              {busy ? <Spinner /> : <Sparkles />}
              Create and launch
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
