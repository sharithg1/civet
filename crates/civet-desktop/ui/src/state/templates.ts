// Worktree templates (civetd >= 0.4.2): per-project setup script, files
// copied from the main checkout, default agent command and first prompt.
// Saved on the host (so `civet handoff` uses them too) and merged there
// with the repository's `.civet/worktree.toml`; this keeps the last reply
// per project for the New worktree dialog and Settings → Projects.

import { create } from 'zustand'
import { CivetError } from '@/backend'
import type { WorktreeTemplate, WorktreeTemplateInfo } from '@/types'
import { versionAtLeast } from './fileops'
import type { StepKind } from './prefs'
import { useStore, wtKey } from './store'

/** Oldest civetd with worktree templates (civet_proto::WORKTREE_TEMPLATE_MIN_DAEMON_VERSION). */
export const WORKTREE_TEMPLATE_MIN_DAEMON_VERSION = '0.4.2'

export const EMPTY_TEMPLATE: WorktreeTemplate = {
  setup: null,
  copy: null,
  agent: null,
  prompt: null
}

export type TemplateView = { info: WorktreeTemplateInfo | null; error: string | null }

/** By `wtKey(hostId, repo)`. */
export const useTemplates = create<Record<string, TemplateView>>(() => ({}))

export function hasWorktreeTemplates(hostId: string): boolean {
  const h = useStore.getState().hosts[hostId]
  return (
    h?.state === 'connected' &&
    !!h.daemonVersion &&
    versionAtLeast(h.daemonVersion, WORKTREE_TEMPLATE_MIN_DAEMON_VERSION)
  )
}

function put(hostId: string, repo: string, view: TemplateView): void {
  useTemplates.setState({ [wtKey(hostId, repo)]: view })
}

/** Fetch a project's template (null if the daemon is too old or it failed). */
export async function loadTemplate(
  hostId: string,
  repo: string
): Promise<WorktreeTemplateInfo | null> {
  const b = useStore.getState().backend
  if (!b || !hasWorktreeTemplates(hostId)) return null
  try {
    const info = await b.worktreeTemplate(hostId, repo)
    put(hostId, repo, { info, error: null })
    return info
  } catch (err) {
    const prev = useTemplates.getState()[wtKey(hostId, repo)]
    put(hostId, repo, { info: prev?.info ?? null, error: CivetError.from(err).message })
    return null
  }
}

/** Save the host template; throws so the editor can show why. */
export async function saveTemplate(
  hostId: string,
  repo: string,
  template: WorktreeTemplate
): Promise<WorktreeTemplateInfo> {
  const b = useStore.getState().backend
  if (!b) throw new CivetError('notConnected', `${hostId} is not connected`)
  const info = await b.setWorktreeTemplate(hostId, repo, template)
  put(hostId, repo, { info, error: null })
  return info
}

/** One-line summary of what a template does, for the dialog and Settings. */
export function describeTemplate(t: WorktreeTemplate): string {
  const parts: string[] = []
  const lines = (t.setup ?? '')
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
  if (lines.length) parts.push(lines.length > 1 ? `${lines[0]} (+${lines.length - 1})` : lines[0]!)
  if (t.copy?.length) parts.push(`copies ${t.copy.join(', ')}`)
  return parts.join(' · ')
}

/** The Settings editor's text fields. */
export type Draft = { setup: string; copy: string; agent: string; prompt: string }

/** The editor's text fields for a host template (null = empty). */
export function draftOf(t: WorktreeTemplate): Draft {
  return {
    setup: t.setup ?? '',
    copy: (t.copy ?? []).join('\n'),
    agent: t.agent ?? '',
    prompt: t.prompt ?? ''
  }
}

/** A host template from the editor: blank fields are unset (the repository file applies). */
export function templateOf(d: Draft): WorktreeTemplate {
  const text = (s: string) => (s.trim() ? s : null)
  const copy = d.copy
    .split(/[\n,]/)
    .map((p) => p.trim())
    .filter(Boolean)
  return {
    setup: text(d.setup.replace(/\s+$/, '')),
    copy: copy.length ? copy : null,
    agent: text(d.agent.trim()),
    prompt: text(d.prompt.trim())
  }
}

/** The on-create step kind of an agent command line (`codex …` is Codex, anything else Claude). */
export function agentStepKind(cmd: string): StepKind {
  const program = cmd.trim().split(/\s+/)[0] ?? ''
  return program.split('/').pop() === 'codex' ? 'codex' : 'agent'
}
