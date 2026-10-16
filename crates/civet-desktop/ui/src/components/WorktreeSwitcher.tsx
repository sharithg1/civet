// ⌘K worktree switcher, also opened from the title bar's worktree selector
// and the sidebar's Search. Rows, order and sections come from
// lib/switcher.ts: current + recent first when the query is empty, fuzzy
// matches on name, task, project and directory otherwise. ↑↓ / Tab move,
// ⏎ opens, Esc closes and hands focus back to the terminal.
import { useEffect, useMemo, useRef, useState } from 'react'
import { Laptop, Server } from 'lucide-react'
import { allWorktrees, panesOfWorktree, selectWorktree, useStore, wsKey } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { switcherGroups, type SwitcherEntry } from '@/lib/switcher'
import {
  ago,
  highlightRuns,
  shortPath,
  worktreeBranch,
  worktreeDot,
  worktreeTask,
  type DotKind
} from '@/lib/worktree-view'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { ProjectTile, RingGlyph } from './chrome'
import { StatusDot } from './StatusDot'

type Item = SwitcherEntry & {
  worktree: string
  isMain: boolean
  /** A folder project's root (no branch). */
  folder: boolean
  dot: DotKind
  tabs: number
}

export function WorktreeSwitcher() {
  const open = useStore((s) => s.switcherOpen)
  return (
    <Dialog open={open} onOpenChange={(o) => useStore.setState({ switcherOpen: o })}>
      <DialogContent
        showCloseButton={false}
        overlayClassName="bg-black/50"
        className="top-[13vh] w-[580px] max-w-[calc(100vw-32px)] translate-y-0 gap-0 overflow-hidden rounded-xl p-0 shadow-palette sm:max-w-[580px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <DialogTitle className="sr-only">Switch worktree</DialogTitle>
        <DialogDescription className="sr-only">
          Type to filter by task, branch or project.
        </DialogDescription>
        {open && <SwitcherBody />}
      </DialogContent>
    </Dialog>
  )
}

function SwitcherBody() {
  const [q, setQ] = useState('')
  const [idx, setIdx] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)
  const hosts = useStore((s) => s.hosts)
  const repos = useStore((s) => s.repos)
  const worktrees = useStore((s) => s.worktrees)
  const panes = useStore((s) => s.panes)
  const workspaces = useStore((s) => s.workspaces)
  const unread = useStore((s) => s.unread)
  const active = useStore((s) => s.active)
  const mru = useStore((s) => s.mru)
  const seen = useStore((s) => s.seen)
  const recentCount = usePrefs((s) => s.recentCount)
  const currentKey = active ? wsKey(active.hostId, active.worktree) : null

  const items = useMemo(() => {
    void worktrees // recompute when worktrees change (allWorktrees reads the store)
    return allWorktrees().map(({ hostId, wt }): Item => {
      const wtPanes = panesOfWorktree(panes[hostId], wt.id)
      const key = wsKey(hostId, wt.id)
      return {
        key,
        hostId,
        worktree: wt.id,
        repo: wt.repo,
        name: worktreeBranch(wt),
        path: wt.path,
        project: repos[hostId]?.find((r) => r.id === wt.repo)?.name ?? wt.repo,
        host: hosts[hostId]?.label ?? hostId,
        local: hostId === 'local',
        task: wt.isMain ? null : worktreeTask(wt, wtPanes),
        isMain: wt.isMain,
        folder: repos[hostId]?.find((r) => r.id === wt.repo)?.kind === 'folder',
        dot: worktreeDot(wtPanes, hosts[hostId]?.state === 'connected', unread[key]),
        tabs: workspaces[key]?.tabs.length || wtPanes.length
      }
    })
  }, [hosts, repos, worktrees, panes, workspaces, unread])

  const groups = useMemo(
    () => switcherGroups(items, { query: q, mru, currentKey, recentCount }),
    [items, q, mru, currentKey, recentCount]
  )
  const flat = useMemo(() => groups.flatMap((g) => g.rows), [groups])

  const sel = Math.min(idx, Math.max(0, flat.length - 1))
  useEffect(() => {
    listRef.current?.querySelector(`[data-sw="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const open = (it: Item | undefined) => it && selectWorktree(it.hostId, it.worktree)
  const move = (d: number) => flat.length && setIdx((sel + d + flat.length) % flat.length)

  let n = 0
  return (
    <>
      <div className="flex h-[46px] items-center gap-2.5 border-b border-border px-3.5">
        <RingGlyph size={11} />
        <input
          autoFocus
          value={q}
          spellCheck={false}
          aria-label="Switch to worktree"
          placeholder="Switch to worktree…  (name, task, or project)"
          onChange={(e) => {
            setQ(e.target.value)
            setIdx(0)
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown' || (e.key === 'Tab' && !e.shiftKey)) {
              e.preventDefault()
              move(1)
            } else if (e.key === 'ArrowUp' || (e.key === 'Tab' && e.shiftKey)) {
              e.preventDefault()
              move(-1)
            } else if (e.key === 'Enter') {
              e.preventDefault()
              open(flat[sel])
            }
          }}
          className="min-w-0 flex-1 border-none bg-transparent text-base text-foreground outline-none placeholder:text-faint"
        />
        <span className="shrink-0 font-mono text-2xs text-faint">
          {flat.length} worktree{flat.length === 1 ? '' : 's'}
        </span>
      </div>
      <div ref={listRef} className="max-h-[min(440px,58vh)] overflow-y-auto p-1" role="listbox">
        {groups.map((g, gi) => (
          <div key={`${g.label}:${g.host}:${gi}`} className="pb-1">
            {g.label && (
              <div className="flex items-center gap-[7px] px-2 pt-2 pb-1 text-xs font-medium text-muted-foreground">
                {g.project && <ProjectTile name={g.label} size={10} />}
                {g.label}
                {g.host && <span className="font-normal text-dim">{g.host}</span>}
              </div>
            )}
            {g.rows.map((it) => {
              const i = n++
              const meta = `${it.current ? 'current' : seen[it.key] ? ago(seen[it.key]!) : '—'} · ${it.tabs} tab${it.tabs === 1 ? '' : 's'}`
              return (
                <div
                  key={it.key}
                  data-sw={i}
                  role="option"
                  aria-selected={i === sel}
                  aria-current={it.current || undefined}
                  onClick={() => open(it)}
                  onMouseMove={() => i !== sel && setIdx(i)}
                  className={cn(
                    'flex cursor-default items-center gap-2.5 rounded-md px-2 py-1.5',
                    i === sel && 'bg-accent'
                  )}
                >
                  <StatusDot kind={it.dot} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-[7px]">
                      <span className="min-w-0 truncate font-mono text-sm">
                        <Hl
                          text={it.name}
                          idxs={it.hits.name}
                          base={it.current ? 'text-foreground' : 'text-body'}
                        />
                      </span>
                      {it.isMain && (
                        <span className="shrink-0 rounded-sm bg-muted px-[5px] text-2xs leading-4 font-medium text-body">
                          {it.folder ? 'folder' : 'default'}
                        </span>
                      )}
                    </div>
                    <div className="flex min-w-0 items-center gap-1 truncate text-xs leading-4 text-faint">
                      {!g.project && (
                        <>
                          <Hl
                            text={it.project}
                            idxs={it.hits.project}
                            base="text-faint"
                            hit="text-body"
                          />
                          <span className="text-dim">·</span>
                          {it.local ? (
                            <Laptop className="size-3 shrink-0" aria-label="Local" />
                          ) : (
                            <Server className="size-3 shrink-0" aria-label="Remote" />
                          )}
                          <span className="shrink-0">{it.host}</span>
                        </>
                      )}
                      {g.project && !it.task && (
                        <span className="truncate font-mono">{shortPath(it.path)}</span>
                      )}
                      {it.task && (
                        <>
                          {!g.project && <span className="text-dim">·</span>}
                          <span className="min-w-0 truncate">
                            <Hl
                              text={it.task}
                              idxs={it.hits.task}
                              base="text-faint"
                              hit="text-body"
                            />
                          </span>
                        </>
                      )}
                    </div>
                  </div>
                  <span
                    className={cn(
                      'shrink-0 font-mono text-2xs whitespace-nowrap',
                      it.current ? 'text-muted-foreground' : 'text-faint'
                    )}
                  >
                    {meta}
                  </span>
                  <span className="w-3.5 shrink-0 text-right font-mono text-2xs text-muted-foreground">
                    {i === sel ? '⏎' : ''}
                  </span>
                </div>
              )
            })}
          </div>
        ))}
        {flat.length === 0 && (
          <div className="p-[22px] text-center text-sm text-faint">No worktrees match.</div>
        )}
      </div>
      <div className="flex gap-4 border-t border-border px-3.5 py-2 font-mono text-2xs text-faint">
        <span>↑↓ move</span>
        <span>⏎ open</span>
        <span>⌥` previous worktree</span>
        <span className="flex-1" />
        <span>esc</span>
      </div>
    </>
  )
}

function Hl({
  text,
  idxs,
  base,
  hit = 'text-foreground font-semibold'
}: {
  text: string
  idxs: number[]
  base: string
  hit?: string
}) {
  if (!idxs.length) return <span className={base}>{text}</span>
  return (
    <>
      {highlightRuns(text, idxs).map((r, i) => (
        <span key={i} className={r.hit ? hit : base}>
          {r.s}
        </span>
      ))}
    </>
  )
}
