// Right panel of the v3 workspace (Files / Changes / Branch). civetd does not
// expose a worktree's files, git status, diffs, commits or PRs yet, so Files
// and Changes are explanatory placeholders and Branch shows what the daemon
// does report (branch, HEAD, path, agents) plus "Remove worktree…".
// Backend follow-up: list_files / git_status / git_diff / branch_info.
import { panesOfWorktree, pushToast, useStore, type RightTab } from '@/state/store'
import { cn } from '@/lib/utils'
import { DOT_LABEL, paneDot, splitBranch, worktreeBranch } from '@/lib/worktree-view'
import { useWorktreeInfo } from './chrome'
import { StatusDot } from './StatusDot'

const TABS: [RightTab, string][] = [
  ['files', 'Files'],
  ['changes', 'Changes'],
  ['branch', 'Branch']
]

export function RightPanel() {
  const active = useStore((s) => s.active)
  const tab = useStore((s) => s.rightTab)
  return (
    <aside className="flex min-h-0 w-[300px] shrink-0 flex-col border-l border-border bg-chrome">
      <div className="flex h-9 shrink-0 items-stretch border-b border-border px-1.5" role="tablist">
        {TABS.map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={tab === id}
            onClick={() => useStore.setState({ rightTab: id })}
            className={cn(
              'relative flex cursor-pointer items-center gap-1.5 px-2.5 text-[12px] hover:text-foreground',
              tab === id ? 'text-foreground' : 'text-quiet'
            )}
          >
            {label}
            {tab === id && <span aria-hidden className="absolute inset-x-2 -bottom-px h-0.5 bg-soft" />}
          </button>
        ))}
      </div>
      {active ? (
        <PanelBody hostId={active.hostId} worktree={active.worktree} tab={tab} />
      ) : (
        <Note>Select a worktree.</Note>
      )}
    </aside>
  )
}

function Note({ children }: { children: React.ReactNode }) {
  return <div className="px-2 py-1.5 text-[12px] leading-[1.5] text-faint">{children}</div>
}

function PanelBody({ hostId, worktree, tab }: { hostId: string; worktree: string; tab: RightTab }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const host = useStore((s) => s.hosts[hostId])
  const paneMap = useStore((s) => s.panes[hostId])
  if (!wt) return null
  const { repo, info } = wt
  const branch = worktreeBranch(info)
  const { prefix, name } = splitBranch(branch)
  const connected = host?.state === 'connected'
  const remote = `${host?.label ?? hostId}:${info.path}`
  const panes = panesOfWorktree(paneMap, worktree).sort((a, b) => a.id - b.id)
  return (
    <>
      <div className="flex shrink-0 flex-col gap-2 px-3 pt-2.5 pb-2">
        <div className="flex items-center gap-1.5 text-[12.5px]">
          <span className="shrink-0 font-medium whitespace-nowrap text-foreground">{repo.name}</span>
          <span className="text-dim">/</span>
          <span className="min-w-0 truncate font-mono text-[11.5px] text-muted-foreground">
            {prefix}
            {name}
          </span>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
        {tab === 'files' && (
          <Note>
            File browsing isn&apos;t available yet: civetd doesn&apos;t list a worktree&apos;s files. Use a
            Terminal tab (⌥T) meanwhile.
          </Note>
        )}
        {tab === 'changes' && (
          <Note>
            Git status and diffs aren&apos;t reported by civetd yet. Run{' '}
            <span className="font-mono text-[11px] text-muted-foreground">git status</span> in a Terminal tab.
          </Note>
        )}
        {tab === 'branch' && (
          <div className="flex flex-col gap-2.5 px-2 py-0.5 text-[12.5px]">
            <div className="flex flex-col gap-1.5">
              <div>
                <span className="font-mono text-muted-foreground">{info.head ? info.head.slice(0, 7) : '—'}</span>{' '}
                <span className="text-foreground">{info.isMain ? 'Default checkout' : branch}</span>
              </div>
              <div className="text-muted-foreground">
                {info.isMain ? `tracks ${repo.defaultBranch ?? 'the default branch'}` : `from ${repo.name}`}
                {info.locked ? ' · locked' : ''}
              </div>
            </div>
            <Section title="Sessions">
              {panes.length === 0 && <div className="text-muted-foreground">No sessions running</div>}
              {panes.map((p) => {
                const dot = paneDot(p, connected)
                return (
                  <div key={p.id} className="flex items-center gap-2" title={DOT_LABEL[dot]}>
                    {p.agent ? <StatusDot kind={dot} /> : <span className="font-mono text-[10px] text-faint">›_</span>}
                    <span className="min-w-0 truncate text-soft">{p.title || p.command.join(' ')}</span>
                    <span className="ml-auto shrink-0 font-mono text-[10.5px] text-faint">
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
                onClick={() => {
                  void navigator.clipboard?.writeText(info.path).catch(() => {})
                  pushToast({ kind: 'info', title: `Copied ${remote}` }, 3200)
                }}
                className="cursor-copy text-left font-mono text-[11.5px] break-all text-muted-foreground"
              >
                {remote}
              </button>
            </Section>
            <Section title="Pull request">
              <div className="text-muted-foreground">PR and unpushed-commit status aren&apos;t reported by civetd yet.</div>
            </Section>
            {!info.isMain && (
              <button
                type="button"
                disabled={!connected}
                onClick={() => useStore.setState({ removeWorktree: { hostId, worktree } })}
                className="h-[26px] cursor-pointer self-start rounded-[5px] border border-border px-2.5 text-[12px] text-soft hover:bg-hover disabled:cursor-default disabled:opacity-50"
              >
                Remove worktree…
              </button>
            )}
          </div>
        )}
      </div>
    </>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-[5px] border-t border-border pt-2.5">
      <div className="text-[11.5px] text-faint">{title}</div>
      {children}
    </div>
  )
}
