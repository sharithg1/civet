// ⌘K worktree switcher (v3): Recent (MRU) + one group per project when the
// query is empty; otherwise fuzzy matches on task, branch and project,
// grouped by project and ordered by best score. ↑↓ / Tab move, ⏎ opens, Esc
// closes and hands focus back to the terminal.
import { useEffect, useMemo, useRef, useState } from 'react'
import { allWorktrees, panesOfWorktree, selectWorktree, useStore, wsKey } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import {
  ago,
  highlightRuns,
  matchFields,
  worktreeBranch,
  worktreeDot,
  worktreeTask,
  type DotKind
} from '@/lib/worktree-view'
import type { WorktreeInfo } from '@/types'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { ProjectTile, RingGlyph } from './chrome'
import { StatusDot } from './StatusDot'

type Item = {
  key: string
  hostId: string
  wt: WorktreeInfo
  project: string
  primary: string
  primaryIsTask: boolean
  secondary: string
  dot: DotKind
  tabs: number
  hits: number[][]
  score: number
}

type Group = { label: string; sub: string; tile: string | null; items: Item[] }

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
        <DialogDescription className="sr-only">Type to filter by task, branch or project.</DialogDescription>
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
  const labelPref = usePrefs((s) => s.worktreeLabel)
  const currentKey = active ? wsKey(active.hostId, active.worktree) : null

  const { groups, flat } = useMemo(() => {
    void worktrees // recompute when worktrees change (allWorktrees reads the store)
    const tokens = q.trim().toLowerCase().split(/\s+/).filter(Boolean)
    const items: Item[] = []
    for (const { hostId, wt } of allWorktrees()) {
      const project = repos[hostId]?.find((r) => r.id === wt.repo)?.name ?? wt.repo
      const wtPanes = panesOfWorktree(panes[hostId], wt.id)
      const branch = worktreeBranch(wt)
      const task = wt.isMain ? null : worktreeTask(wt, wtPanes)
      const primaryIsTask = labelPref === 'task' && !!task
      const primary = wt.isMain ? branch : primaryIsTask ? task! : branch
      const secondary = wt.isMain ? 'default checkout' : primaryIsTask ? branch : (task ?? wt.path)
      const fields = [primary, secondary, project]
      const m = tokens.length ? matchFields(tokens, fields) : { score: 0, hits: [[], [], []] }
      if (!m) continue
      const key = wsKey(hostId, wt.id)
      const ws = workspaces[key]
      items.push({
        key,
        hostId,
        wt,
        project,
        primary,
        primaryIsTask,
        secondary,
        dot: worktreeDot(wtPanes, hosts[hostId]?.state === 'connected', unread[key]),
        tabs: ws?.tabs.length || wtPanes.length,
        hits: m.hits,
        score: m.score
      })
    }
    const groups: Group[] = []
    const projectKey = (it: Item) => `${it.hostId}\u0000${it.wt.repo}`
    const hostLabel = (h: string) => hosts[h]?.label ?? h
    if (!tokens.length) {
      const recent = mru
        .filter((k) => k !== currentKey)
        .map((k) => items.find((it) => it.key === k))
        .filter((x): x is Item => !!x)
        .slice(0, recentCount)
      if (recent.length) groups.push({ label: 'Recent', sub: '', tile: null, items: recent })
      const byProject = new Map<string, Item[]>()
      for (const it of items) {
        if (recent.includes(it)) continue
        byProject.set(projectKey(it), [...(byProject.get(projectKey(it)) ?? []), it])
      }
      for (const list of byProject.values()) {
        groups.push({ label: list[0]!.project, sub: hostLabel(list[0]!.hostId), tile: list[0]!.project, items: list })
      }
    } else {
      const rank = (k: string) => {
        const i = mru.indexOf(k)
        return i < 0 ? 1e9 : i
      }
      const byProject = new Map<string, Item[]>()
      for (const it of items) byProject.set(projectKey(it), [...(byProject.get(projectKey(it)) ?? []), it])
      const sorted = [...byProject.values()]
        .map((list) => list.sort((a, b) => b.score - a.score || rank(a.key) - rank(b.key)))
        .sort((a, b) => b[0]!.score - a[0]!.score)
      for (const list of sorted) {
        groups.push({ label: list[0]!.project, sub: hostLabel(list[0]!.hostId), tile: list[0]!.project, items: list })
      }
    }
    return { groups, flat: groups.flatMap((g) => g.items) }
  }, [q, hosts, repos, worktrees, panes, workspaces, unread, mru, currentKey, recentCount, labelPref])

  const sel = Math.min(idx, Math.max(0, flat.length - 1))
  useEffect(() => {
    listRef.current?.querySelector(`[data-sw="${sel}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const open = (it: Item | undefined) => it && selectWorktree(it.hostId, it.wt.id)
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
          placeholder="Switch to worktree…  (task, branch, or project)"
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
          className="min-w-0 flex-1 border-none bg-transparent text-[14px] text-foreground outline-none placeholder:text-faint"
        />
        <span className="shrink-0 font-mono text-[10.5px] text-faint">
          {flat.length} worktree{flat.length === 1 ? '' : 's'}
        </span>
      </div>
      <div ref={listRef} className="max-h-[min(440px,58vh)] overflow-y-auto p-1" role="listbox">
        {groups.map((g) => (
          <div key={`${g.label}:${g.sub}`} className="pb-1">
            <div className="flex items-center gap-[7px] px-2 pt-2 pb-1 text-[11.5px] font-medium text-muted-foreground">
              {g.tile && <ProjectTile name={g.tile} size={10} />}
              {g.label}
              <span className="font-normal text-dim">{g.sub}</span>
            </div>
            {g.items.map((it) => {
              const i = n++
              const current = it.key === currentKey
              const meta = `${g.label === 'Recent' ? `${it.project} · ` : ''}${current ? 'current' : seen[it.key] ? ago(seen[it.key]!) : '—'} · ${it.tabs} tab${it.tabs === 1 ? '' : 's'}`
              return (
                <div
                  key={it.key}
                  data-sw={i}
                  role="option"
                  aria-selected={i === sel}
                  onClick={() => open(it)}
                  onMouseMove={() => i !== sel && setIdx(i)}
                  className={cn('flex cursor-default items-center gap-2.5 rounded-md px-2 py-1.5', i === sel && 'bg-accent')}
                >
                  <StatusDot kind={it.dot} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-[7px]">
                      <span
                        className={cn(
                          'min-w-0 truncate',
                          it.primaryIsTask ? 'text-[13px]' : 'font-mono text-[12.5px]'
                        )}
                      >
                        <Hl text={it.primary} idxs={it.hits[0]!} base="text-body" />
                      </span>
                      {it.wt.isMain && (
                        <span className="shrink-0 rounded-sm bg-muted px-[5px] text-[10px] leading-4 font-medium text-body">
                          default
                        </span>
                      )}
                    </div>
                    <div
                      className={cn(
                        'truncate text-[11px] leading-4',
                        (it.primaryIsTask || it.wt.isMain) && 'font-mono'
                      )}
                    >
                      <Hl text={it.secondary} idxs={it.hits[1]!} base="text-faint" hit="text-body" />
                    </div>
                  </div>
                  <span className="shrink-0 font-mono text-[10.5px] whitespace-nowrap text-faint">{meta}</span>
                  <span className="w-3.5 shrink-0 text-right font-mono text-[10.5px] text-muted-foreground">
                    {i === sel ? '⏎' : ''}
                  </span>
                </div>
              )
            })}
          </div>
        ))}
        {flat.length === 0 && <div className="p-[22px] text-center text-[13px] text-faint">No worktrees match.</div>}
      </div>
      <div className="flex gap-4 border-t border-border px-3.5 py-2 font-mono text-[10.5px] text-faint">
        <span>↑↓ move</span>
        <span>⏎ open</span>
        <span>⌥` previous worktree</span>
        <span className="flex-1" />
        <span>esc</span>
      </div>
    </>
  )
}

function Hl({ text, idxs, base, hit = 'text-foreground font-semibold' }: { text: string; idxs: number[]; base: string; hit?: string }) {
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
