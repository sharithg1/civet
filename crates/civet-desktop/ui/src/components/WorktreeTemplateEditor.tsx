// Settings → Projects → "Worktree setup": a project's worktree template as
// saved on the host. Each field left empty falls back to the repository's
// `.civet/worktree.toml` (shown as the placeholder, with where it came
// from); a filled one overrides it. Saved explicitly, since it is written
// on the host (where `civet handoff` reads it too).

import { useEffect, useState, type ReactNode } from 'react'
import { CivetError } from '@/backend'
import { cn } from '@/lib/utils'
import {
  type Draft,
  draftOf,
  EMPTY_TEMPLATE,
  loadTemplate,
  saveTemplate,
  templateOf,
  useTemplates
} from '@/state/templates'
import { wtKey } from '@/state/store'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'

const FIELD =
  'rounded-md border-border bg-transparent px-2 font-mono text-sm shadow-none dark:bg-transparent placeholder:text-faint'

const btn =
  'h-7 cursor-pointer rounded-md border border-border px-2.5 text-sm text-soft hover:bg-hover hover:text-foreground disabled:cursor-default disabled:opacity-50'

export function WorktreeTemplateEditor({ hostId, repo }: { hostId: string; repo: string }) {
  const view = useTemplates((s) => s[wtKey(hostId, repo)])
  const info = view?.info ?? null
  const [draft, setDraft] = useState<Draft | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    void loadTemplate(hostId, repo)
  }, [hostId, repo])
  // Start editing from the host's saved template once it is known.
  useEffect(() => {
    if (info && !draft) setDraft(draftOf(info.host))
  }, [info, draft])

  if (!info) {
    return (
      <div className="px-1 pb-3 text-xs text-faint">
        {view?.error ?? 'Loading the worktree template…'}
      </div>
    )
  }
  const d = draft ?? draftOf(info.host)
  const file = info.file ?? EMPTY_TEMPLATE
  const dirty = JSON.stringify(templateOf(d)) !== JSON.stringify(info.host)
  const set = (patch: Partial<Draft>) => {
    setSaved(false)
    setDraft({ ...d, ...patch })
  }
  const fromFile = (has: boolean) =>
    has ? 'Empty: the greyed value from .civet/worktree.toml.' : 'Empty: none.'

  async function save() {
    setBusy(true)
    setError(null)
    try {
      const next = await saveTemplate(hostId, repo, templateOf(d))
      setDraft(draftOf(next.host))
      setSaved(true)
    } catch (err) {
      setError(CivetError.from(err).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex flex-col gap-3 px-1 pb-3" aria-label="Worktree setup">
      <div className="text-xs text-muted-foreground">
        Applied to each new worktree (and to <span className="font-mono">civet handoff</span>).
        Fields set here override the repository file{' '}
        <span className="font-mono text-faint">{info.filePath}</span>
        {info.file ? '' : ' (none yet)'}.
        {info.fileError && <div className="mt-1 text-status-err-text">{info.fileError}</div>}
      </div>
      <Field
        label="Setup script"
        hint={`Runs in a Setup tab (sh -e, or its #! line). ${fromFile(!!file.setup)}`}
      >
        <Textarea
          aria-label="Setup script"
          rows={3}
          spellCheck={false}
          value={d.setup}
          placeholder={file.setup ?? 'pnpm install'}
          onChange={(e) => set({ setup: e.target.value })}
          className={cn('field-sizing-fixed min-h-0 resize-y py-1.5 text-xs', FIELD)}
        />
      </Field>
      <Field
        label="Copy from the main checkout"
        hint={`One path per line; * and ? match within a name; existing files are kept. ${fromFile(!!file.copy)}`}
      >
        <Textarea
          aria-label="Copy from the main checkout"
          rows={2}
          spellCheck={false}
          value={d.copy}
          placeholder={file.copy?.join('\n') ?? '.env\n.env.local'}
          onChange={(e) => set({ copy: e.target.value })}
          className={cn('field-sizing-fixed min-h-0 resize-y py-1.5 text-xs', FIELD)}
        />
      </Field>
      <Field
        label="Agent command"
        hint={`The first agent of a new worktree. ${fromFile(!!file.agent)}`}
      >
        <Input
          aria-label="Agent command"
          spellCheck={false}
          value={d.agent}
          placeholder={file.agent ?? 'claude'}
          onChange={(e) => set({ agent: e.target.value })}
          className={cn('h-7 text-xs', FIELD)}
        />
      </Field>
      <Field
        label="First prompt"
        hint={`Prefills the first task; put before a handoff's brief. ${fromFile(!!file.prompt)}`}
      >
        <Textarea
          aria-label="First prompt"
          rows={2}
          value={d.prompt}
          placeholder={file.prompt ?? 'Read CONTRIBUTING.md before you start.'}
          onChange={(e) => set({ prompt: e.target.value })}
          className={cn('field-sizing-fixed min-h-0 resize-y py-1.5 font-sans text-xs', FIELD)}
        />
      </Field>
      <div className="flex items-center gap-2">
        <button type="button" className={btn} disabled={busy || !dirty} onClick={() => void save()}>
          Save
        </button>
        <button
          type="button"
          className={btn}
          disabled={busy || !dirty}
          onClick={() => setDraft(draftOf(info.host))}
        >
          Revert
        </button>
        <span className="text-xs text-faint">
          {error ? (
            <span className="text-status-err-text">{error}</span>
          ) : saved && !dirty ? (
            'Saved on the host'
          ) : dirty ? (
            'Unsaved changes'
          ) : (
            ''
          )}
        </span>
      </div>
    </div>
  )
}

function Field({ label, hint, children }: { label: string; hint: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-sm text-foreground">{label}</span>
      {children}
      <span className="text-2xs text-faint">{hint}</span>
    </div>
  )
}
