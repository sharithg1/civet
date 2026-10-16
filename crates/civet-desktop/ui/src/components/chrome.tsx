// Small v3 chrome primitives shared by the title bar, sidebar, switcher and
// dialogs: project tile, keycap, the drawn panel-toggle and search glyphs,
// and the worktree tab list derived from the store.
import { leaves } from '@/layout/pane-tree'
import { cn } from '@/lib/utils'
import { paneWhat, tabLabels, tileColor } from '@/lib/worktree-view'
import { isPaneTab, useStore, wsKey, wtKey, type FileTab, type Tab } from '@/state/store'
import type { PaneId, PaneInfo, RepoInfo, WorktreeInfo } from '@/types'

/** Coloured square with the project's initial (sidebar 16px, title bar 14px, switcher 10px). */
export function ProjectTile({
  name,
  size = 16,
  className
}: {
  name: string
  size?: 10 | 12 | 14 | 16
  className?: string
}) {
  return (
    <span
      aria-hidden
      className={cn(
        'grid shrink-0 place-items-center font-semibold text-primary-foreground',
        size >= 14 ? 'rounded-sm' : 'rounded-[3px]',
        size === 16 && 'size-4 text-[10px]',
        size === 14 && 'size-3.5 text-[9px]',
        size === 12 && 'size-3',
        size === 10 && 'size-2.5',
        className
      )}
      style={{ background: tileColor(name) }}
    >
      {size >= 14 ? name.slice(0, 1).toUpperCase() : null}
    </span>
  )
}

/** Keycap hint (⌘K). */
export function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        'rounded-sm border border-border bg-muted px-[5px] font-mono text-2xs leading-[17px] font-normal text-muted-foreground',
        className
      )}
    >
      {children}
    </kbd>
  )
}

/** The design's drawn panel glyph: a rounded rect with a filled bar on one side. */
export function PanelGlyph({ side }: { side: 'left' | 'right' }) {
  return (
    <span
      aria-hidden
      className={cn(
        'flex h-[11px] rounded-[2px] border-[1.5px] border-quiet',
        side === 'left' ? 'w-[13px]' : 'w-[14px] justify-end'
      )}
    >
      <span className="h-full w-1 bg-quiet" />
    </span>
  )
}

/** The design's search/switch glyph: a small hollow circle. */
export function RingGlyph({ size = 10 }: { size?: 10 | 11 }) {
  return (
    <span
      aria-hidden
      className={cn(
        'shrink-0 rounded-full border-[1.5px] border-faint',
        size === 10 ? 'size-2.5' : 'size-[11px]'
      )}
    />
  )
}

export type TabView = {
  /** Workspace tab id; null for a pane of a worktree that has not been opened yet. */
  id: string | null
  label: string
  agent: PaneInfo | null
  /** The focused pane; null for a file tab. */
  focused: PaneId | null
  /** A file tab's worktree-relative path. */
  file: string | null
  paneCount: number
  what: { text: string; mono: boolean }
}

/**
 * A worktree's tabs as the chrome shows them. Opened worktrees use their
 * workspace tabs; unopened ones list their live panes (one tab each, which is
 * what selecting the worktree will open).
 */
export function useWorktreeTabs(hostId: string, worktree: string): TabView[] {
  const ws = useStore((s) => s.workspaces[wsKey(hostId, worktree)])
  const panes = useStore((s) => s.panes[hostId])
  return worktreeTabs(ws?.tabs, panes, worktree)
}

export function worktreeTabs(
  tabs: Tab[] | undefined,
  panes: Record<PaneId, PaneInfo> | undefined,
  worktree: string
): TabView[] {
  type Group = { id: string | null; ids: PaneId[]; focused: PaneId }
  const groups: (Group | FileTab)[] = tabs?.length
    ? tabs.map((t) => (isPaneTab(t) ? { id: t.id, ids: leaves(t.root), focused: t.focused } : t))
    : Object.values(panes ?? {})
        .filter((p) => p.worktree === worktree)
        .sort((a, b) => a.id - b.id)
        .map((p) => ({ id: null, ids: [p.id], focused: p.id }))
  const paneGroups = groups.filter((g): g is Group => !('kind' in g))
  const labels = tabLabels(paneGroups.map((g) => g.ids.map((id) => panes?.[id])))
  return groups.map((g) => {
    if ('kind' in g) {
      const slash = g.path.lastIndexOf('/')
      return {
        id: g.id,
        label: g.path.slice(slash + 1),
        agent: null,
        focused: null,
        file: g.path,
        paneCount: 0,
        what: { text: slash < 0 ? '' : g.path.slice(0, slash), mono: true }
      }
    }
    const l = labels[paneGroups.indexOf(g)]!
    return {
      id: g.id,
      label: l.label,
      agent: l.agent,
      focused: g.focused,
      file: null,
      paneCount: g.ids.length,
      what: paneWhat(l.agent ?? panes?.[g.focused])
    }
  })
}

/** The repo and WorktreeInfo of a worktree id (stable store slices only). */
export function useWorktreeInfo(
  hostId: string,
  worktree: string
): { repo: RepoInfo; info: WorktreeInfo } | null {
  const repos = useStore((s) => s.repos[hostId])
  const worktrees = useStore((s) => s.worktrees)
  for (const repo of repos ?? []) {
    const info = worktrees[wtKey(hostId, repo.id)]?.find((w) => w.id === worktree)
    if (info) return { repo, info }
  }
  return null
}
