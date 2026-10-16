// Right panel (Files / Changes / Branch / Ports), styled after Cursor's review pane:
// its own header row of icon pills with the panel toggle at the far right,
// then a PR-like branch view (state badge, branch → base, title, sections).
// Files browses and previews the worktree's files (FilesTab.tsx); Changes
// lists its git status and per-file diffs (ChangesTab.tsx; the tab label's
// count keeps polling while the panel is open). civetd does not expose
// commits or PRs yet, so Branch shows what the daemon does report (branch,
// HEAD, path, sessions) plus "Remove worktree…". Ports lists listening ports
// and port forwards (PortsTab.tsx).
// Backend follow-up: branch_info.
import { ArrowRight, ArrowRightLeft, Copy, Diff, FileText, GitBranch, Terminal } from 'lucide-react'
import { panesOfWorktree, pushToast, useStore, type RightTab } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import { cn } from '@/lib/utils'
import { DOT_LABEL, paneDot, worktreeBranch, worktreeTask } from '@/lib/worktree-view'
import { useWorktreeInfo } from './chrome'
import { usePorts } from '@/state/ports'
import { IconButton } from './IconButton'
import { FilesTab } from './FilesTab'
import { ChangesCount, ChangesTab } from './ChangesTab'
import { PortsTab } from './PortsTab'
import { StatusDot } from './StatusDot'
import { RightPanelToggle } from './TitleBar'

const TABS: [RightTab, string, typeof FileText][] = [
  ['files', 'Files', FileText],
  ['changes', 'Changes', Diff],
  ['branch', 'Branch', GitBranch],
  ['ports', 'Ports', ArrowRightLeft]
]

export function RightPanel() {
  const active = useStore((s) => s.active)
  const tab = useStore((s) => s.rightTab)
  const forwardCount = usePorts((s) => (active ? (s.forwards[active.hostId]?.length ?? 0) : 0))
  return (
    <aside className="flex min-h-0 w-[360px] shrink-0 flex-col border-l border-border bg-background">
      <div
        data-tauri-drag-region="deep"
        className="flex h-10 shrink-0 items-center gap-0.5 px-2 select-none"
      >
        <div className="flex items-center gap-0.5" role="tablist">
          {TABS.map(([id, label, Icon]) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={tab === id}
              onClick={() => useStore.setState({ rightTab: id })}
              className={cn(
                'flex h-7 cursor-pointer items-center gap-1.5 rounded-md px-2 text-base',
                tab === id ? 'bg-card text-foreground' : 'text-quiet hover:bg-hover hover:text-soft'
              )}
            >
              <Icon className="size-3.5 shrink-0" />
              {label}
              {id === 'changes' && active && (
                <ChangesCount
                  hostId={active.hostId}
                  worktree={active.worktree}
                  showing={tab === 'changes'}
                />
              )}
              {id === 'ports' && forwardCount > 0 && (
                <span className="font-mono text-xs text-quiet">{forwardCount}</span>
              )}
            </button>
          ))}
        </div>
        <div className="flex-1 self-stretch" />
        <RightPanelToggle />
      </div>
      {active ? (
        <PanelBody hostId={active.hostId} worktree={active.worktree} tab={tab} />
      ) : (
        <Empty icon={GitBranch}>Select a worktree.</Empty>
      )}
    </aside>
  )
}

function Empty({ icon: Icon, children }: { icon: typeof FileText; children: React.ReactNode }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8 pb-16 text-center text-sm text-quiet">
      <Icon className="size-5 text-dim" />
      <div className="max-w-[260px] leading-relaxed">{children}</div>
    </div>
  )
}

function PanelBody({ hostId, worktree, tab }: { hostId: string; worktree: string; tab: RightTab }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const host = useStore((s) => s.hosts[hostId])
  const paneMap = useStore((s) => s.panes[hostId])
  const labelPref = usePrefs((s) => s.worktreeLabel)
  if (!wt) return null
  const connected = host?.state === 'connected'
  if (tab === 'files') {
    return (
      <FilesTab hostId={hostId} worktree={worktree} root={wt.info.path} connected={connected} />
    )
  }
  if (tab === 'changes') {
    return <ChangesTab key={worktree} hostId={hostId} worktree={worktree} connected={connected} />
  }
  const { repo, info } = wt
  const branch = worktreeBranch(info)
  const remote = `${host?.label ?? hostId}:${info.path}`
  const panes = panesOfWorktree(paneMap, worktree).sort((a, b) => a.id - b.id)
  if (tab === 'ports') {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pt-1 pb-3">
        <PortsTab hostId={hostId} worktree={worktree} panes={panes} connected={connected} />
      </div>
    )
  }
  const task = info.isMain ? null : worktreeTask(info, panes)
  const title = labelPref === 'task' && task ? task : info.isMain ? 'Default checkout' : branch
  const copyPath = () => {
    void navigator.clipboard?.writeText(info.path).catch(() => {})
    pushToast({ kind: 'info', title: `Copied ${remote}` }, 3200)
  }
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="flex flex-col gap-2.5 px-3 pt-1.5 pb-3">
        <div className="flex min-w-0 items-center gap-2 text-sm">
          <span
            className={cn(
              'shrink-0 rounded-[5px] px-1.5 py-px text-sm',
              info.isMain ? 'bg-secondary text-soft' : 'bg-status-run/15 text-link'
            )}
          >
            {info.isMain ? 'Default' : info.locked ? 'Locked' : 'Worktree'}
          </span>
          <span className="min-w-0 truncate font-mono text-xs text-soft">{branch}</span>
          {!info.isMain && repo.defaultBranch && (
            <>
              <ArrowRight className="size-3 shrink-0 text-dim" />
              <span className="shrink-0 font-mono text-xs text-quiet">{repo.defaultBranch}</span>
            </>
          )}
        </div>
        <div className="flex items-start gap-1.5">
          <h2 className="min-w-0 text-lg leading-snug font-medium text-foreground">
            {title}{' '}
            <span className="font-mono text-sm font-normal text-quiet">
              {info.head ? info.head.slice(0, 7) : ''}
            </span>
          </h2>
          <IconButton label="Copy remote path" side="bottom" className="mt-0.5" onClick={copyPath}>
            <Copy className="size-3.5" />
          </IconButton>
        </div>
        <div className="text-sm text-quiet">
          {info.isMain ? `Tracks ${repo.defaultBranch ?? 'the default branch'}` : `In ${repo.name}`}{' '}
          on {host?.label ?? hostId}
        </div>
      </div>

      <Section title="Sessions" count={panes.length}>
        {panes.length === 0 && (
          <div className="px-3 py-2 text-sm text-quiet">No sessions running</div>
        )}
        {panes.map((p) => {
          const dot = paneDot(p, connected)
          return (
            <div
              key={p.id}
              className="flex h-8 items-center gap-2.5 px-3 text-sm"
              title={DOT_LABEL[dot]}
            >
              <span className="grid size-3.5 shrink-0 place-items-center">
                {p.agent ? (
                  <StatusDot kind={dot} size={6} />
                ) : (
                  <Terminal className="size-3.5 text-quiet" />
                )}
              </span>
              <span className="min-w-0 truncate text-soft">{p.title || p.command.join(' ')}</span>
              <span className="ml-auto shrink-0 text-xs text-quiet">
                {p.exited ? `exit ${p.exitCode ?? '?'}` : (p.agent ?? 'shell')}
              </span>
            </div>
          )
        })}
      </Section>

      <Section title="Remote path">
        <button
          type="button"
          title="Copy path"
          onClick={copyPath}
          className="mx-1.5 cursor-copy rounded-md px-1.5 py-1.5 text-left font-mono text-xs break-all text-soft hover:bg-hover"
        >
          {remote}
        </button>
      </Section>

      <Section title="Pull request">
        <div className="px-3 py-2 text-sm text-quiet">
          PR and unpushed-commit status aren&apos;t reported by civetd yet.
        </div>
      </Section>

      {!info.isMain && (
        <div className="px-3 pt-3 pb-4">
          <button
            type="button"
            disabled={!connected}
            onClick={() => useStore.setState({ removeWorktree: { hostId, worktree } })}
            className="h-7 cursor-pointer rounded-md border border-input bg-card px-2.5 text-sm text-soft hover:bg-hover-strong hover:text-foreground disabled:cursor-default disabled:opacity-50"
          >
            Remove worktree…
          </button>
        </div>
      )}
    </div>
  )
}

/** Cursor's "1 File Changed" strip: a hairline-ruled header row above a list. */
function Section({
  title,
  count,
  children
}: {
  title: string
  count?: number
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col pb-1">
      <div className="flex h-9 items-center gap-1.5 border-y border-border px-3 text-sm text-quiet">
        {title}
        {count != null && count > 0 && <span className="text-dim">{count}</span>}
      </div>
      <div className="flex flex-col py-1">{children}</div>
    </div>
  )
}
