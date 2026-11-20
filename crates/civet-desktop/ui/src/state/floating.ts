// Floating workspaces (civetd >= 0.6.3): places for ad-hoc tasks that
// belong to a host, not to one repository. The daemon gives each a folder
// of its own holding a link to every root (repos, worktrees, folders or
// single files anywhere on the host); sessions start in that folder, and
// agents get the root folders as `--add-dir`. To the rest of the UI a
// workspace is a project of kind 'workspace' with one worktree, its folder
// (see isFolderRepo). This is the dialog that creates one or edits its
// roots, and the actions behind it. Named "floating" here because the
// store's `workspaces` are each worktree's tabs.

import { create } from 'zustand'
import type { CivetBackend } from '@/backend'
import { refreshFiles } from './files'
import { versionAtLeast } from './fileops'
import { connectHost, refreshRepos, selectWorktree, useStore, wtKey, type HostView } from './store'

/** Oldest civetd with floating workspaces (civet_proto::WORKSPACES_MIN_DAEMON_VERSION). */
export const WORKSPACES_MIN_DAEMON_VERSION = '0.6.3'

/** The host is connected and its daemon has floating workspaces. */
export function hasFloatingWorkspaces(h: HostView | undefined): boolean {
  return (
    h?.state === 'connected' &&
    (!h.daemonVersion || versionAtLeast(h.daemonVersion, WORKSPACES_MIN_DAEMON_VERSION))
  )
}

function backend(): CivetBackend {
  const b = useStore.getState().backend
  if (!b) throw new Error('backend not ready')
  return b
}

/** The workspace dialog: a new one (location preselected, null: default) or the roots of `repo`. */
export type WorkspaceDialogRequest = { hostId: string | null; repo?: string }

export const useWorkspaceDialog = create<{ request: WorkspaceDialogRequest | null }>(() => ({
  request: null
}))

export function openWorkspaceDialog(hostId: string | null, repo?: string): void {
  useWorkspaceDialog.setState({ request: { hostId, repo } })
}

export function closeWorkspaceDialog(): void {
  useWorkspaceDialog.setState({ request: null })
}

/** The roots a form holds: trimmed, blanks and repeats dropped. */
export function cleanRoots(roots: string[]): string[] {
  return [...new Set(roots.map((r) => r.trim()).filter(Boolean))]
}

/**
 * Create a workspace on a host (starting This Mac's daemon first when
 * needed, like Add project) and open it.
 */
export async function createFloatingWorkspace(
  hostId: string,
  name: string,
  roots: string[]
): Promise<void> {
  if (hostId === 'local' && useStore.getState().hosts.local?.state !== 'connected') {
    await connectHost({ kind: 'local' }, 'auto')
  }
  const workspace = await backend().createWorkspace(hostId, name.trim(), cleanRoots(roots))
  await refreshRepos(hostId)
  const root = useStore.getState().worktrees[wtKey(hostId, workspace.id)]?.[0]
  if (root) selectWorktree(hostId, root.id)
}

/** Replace a workspace's roots and reload its files (the links changed). */
export async function setFloatingWorkspaceRoots(
  hostId: string,
  repo: string,
  roots: string[]
): Promise<void> {
  await backend().setWorkspaceRoots(hostId, repo, cleanRoots(roots))
  await refreshRepos(hostId)
  const root = useStore.getState().worktrees[wtKey(hostId, repo)]?.[0]
  if (root) void refreshFiles(hostId, root.id)
}
