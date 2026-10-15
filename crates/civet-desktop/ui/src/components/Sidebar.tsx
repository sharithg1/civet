// Worktree sidebar: hosts → repos → worktree cards.
// Structure and tokens ported from Orca: src/renderer/src/components/Sidebar.tsx
// and sidebar/WorktreeList.tsx (group headers per repo, worktree-sidebar
// colour tokens, hover-revealed header actions). Orca's filters, kanban,
// virtualised list and SSH target management were dropped.

import { FolderPlus, LogOut, Plus, Server, Sparkles } from 'lucide-react'
import { useStore, disconnectHost, selectWorktree, wtKey, wsKey, panesOfWorktree } from '../state/store'
import { cn } from '../lib/utils'
import type { HostView } from '../state/store'
import { WorktreeCard } from './WorktreeCard'

export function Sidebar() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const backendKind = useStore((s) => s.backend?.kind)
  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-worktree-sidebar-border bg-worktree-sidebar text-worktree-sidebar-foreground">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-worktree-sidebar-border px-3">
        <span className="text-sm font-semibold tracking-tight">Civet</span>
        {backendKind === 'mock' && (
          <span
            className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-500"
            title="Not running inside Tauri: using the in-browser mock backend"
          >
            mock
          </span>
        )}
        <button
          type="button"
          className="ml-auto flex size-6 items-center justify-center rounded text-muted-foreground hover:bg-worktree-sidebar-accent hover:text-foreground"
          title="Connect to another host"
          aria-label="Connect to another host"
          onClick={() => useStore.setState({ showConnect: true })}
        >
          <Plus className="size-4" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-2">
        {hostOrder.map((id) => hosts[id] && <HostSection key={id} host={hosts[id]} />)}
      </div>
    </aside>
  )
}

const STATE_DOT: Record<HostView['state'], string> = {
  connecting: 'bg-yellow-500 animate-pulse',
  connected: 'bg-emerald-500',
  reconnecting: 'bg-amber-500 animate-pulse',
  disconnected: 'bg-neutral-500',
  failed: 'bg-red-500'
}

function HostSection({ host }: { host: HostView }) {
  const repos = useStore((s) => s.repos[host.hostId])
  return (
    <section className="mb-3">
      <div className="group flex h-7 items-center gap-1.5 px-1.5 text-xs">
        <Server className="size-3.5 text-muted-foreground" aria-hidden />
        <span className="min-w-0 flex-1 truncate font-medium" title={host.hostId}>
          {host.label}
        </span>
        <span
          className={cn('size-1.5 rounded-full', STATE_DOT[host.state])}
          title={`${host.state}${host.transport ? ` · ${host.transport === 'quic' ? 'QUIC' : 'SSH tunnel'}` : ''}${host.rttMs != null ? ` · ${host.rttMs} ms` : ''}`}
        />
        <div className="flex items-center opacity-0 transition-opacity group-hover:opacity-100">
          <HeaderButton label="Add repository" onClick={() => useStore.setState({ addRepoFor: host.hostId })}>
            <FolderPlus className="size-3.5" />
          </HeaderButton>
          <HeaderButton label="Disconnect" onClick={() => void disconnectHost(host.hostId)}>
            <LogOut className="size-3.5" />
          </HeaderButton>
        </div>
      </div>
      {host.transport === 'stream' && (
        <div className="px-1.5 pb-1 text-[10px] text-muted-foreground">via SSH tunnel (UDP unavailable)</div>
      )}
      {(repos ?? []).map((repo) => (
        <RepoGroup key={repo.id} hostId={host.hostId} repoId={repo.id} name={repo.name} />
      ))}
      {repos && repos.length === 0 && (
        <button
          type="button"
          className="mx-1.5 mt-1 text-xs text-muted-foreground underline hover:text-foreground"
          onClick={() => useStore.setState({ addRepoFor: host.hostId })}
        >
          Add a repository…
        </button>
      )}
    </section>
  )
}

function RepoGroup({ hostId, repoId, name }: { hostId: string; repoId: string; name: string }) {
  const worktrees = useStore((s) => s.worktrees[wtKey(hostId, repoId)])
  const panes = useStore((s) => s.panes[hostId])
  const active = useStore((s) => s.active)
  const unread = useStore((s) => s.unread)
  return (
    <div className="mb-1.5">
      <div className="group flex h-6 items-center px-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
        <span className="min-w-0 flex-1 truncate">{name}</span>
        <button
          type="button"
          className="flex h-5 items-center gap-1 rounded px-1 normal-case tracking-normal opacity-0 transition-opacity hover:bg-worktree-sidebar-accent hover:text-foreground group-hover:opacity-100"
          title="New worktree + launch agent"
          onClick={() => useStore.setState({ newWorktree: { hostId, repo: repoId } })}
        >
          <Sparkles className="size-3" />
          New
        </button>
      </div>
      <div className="flex flex-col gap-0.5">
        {(worktrees ?? []).map((wt) => (
          <WorktreeCard
            key={wt.id}
            worktree={wt}
            panes={panesOfWorktree(panes, wt.id)}
            active={active?.hostId === hostId && active.worktree === wt.id}
            unread={unread[wsKey(hostId, wt.id)]}
            onSelect={() => selectWorktree(hostId, wt.id)}
          />
        ))}
      </div>
    </div>
  )
}

function HeaderButton({
  label,
  onClick,
  children
}: {
  label: string
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-worktree-sidebar-accent hover:text-foreground"
      title={label}
      aria-label={label}
      onClick={onClick}
    >
      {children}
    </button>
  )
}
