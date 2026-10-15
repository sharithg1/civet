// Worktree sidebar: hosts → repos → worktree cards.
// Structure and tokens ported from Orca: src/renderer/src/components/Sidebar.tsx
// and sidebar/WorktreeList.tsx (group headers per repo, worktree-sidebar
// colour tokens, hover-revealed header actions). Orca's filters, kanban,
// virtualised list and SSH target management were dropped.

import { FolderPlus, LogOut, Plus, Server, Sparkles } from 'lucide-react'
import {
  useStore,
  disconnectHost,
  selectWorktree,
  wtKey,
  wsKey,
  panesOfWorktree
} from '@/state/store'
import type { HostView } from '@/state/store'
import { cn } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { IconButton } from './IconButton'
import { WorktreeCard } from './WorktreeCard'

/** Hover colour for sidebar controls (overrides the ghost button's accent in both themes). */
const SIDEBAR_HOVER = 'hover:bg-worktree-sidebar-accent dark:hover:bg-worktree-sidebar-accent'

export function Sidebar() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const backendKind = useStore((s) => s.backend?.kind)
  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-worktree-sidebar-border bg-worktree-sidebar text-worktree-sidebar-foreground">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-worktree-sidebar-border px-3">
        <span className="text-sm font-semibold tracking-tight">Civet</span>
        {backendKind === 'mock' && (
          <Tooltip>
            <TooltipTrigger asChild>
              <Badge className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-500">
                mock
              </Badge>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              Not running inside Tauri: using the in-browser mock backend
            </TooltipContent>
          </Tooltip>
        )}
        <IconButton
          label="Connect to another host"
          side="bottom"
          className={cn('ml-auto rounded', SIDEBAR_HOVER)}
          onClick={() => useStore.setState({ showConnect: true })}
        >
          <Plus className="size-4" />
        </IconButton>
      </div>
      <ScrollArea
        className="min-h-0 flex-1" // Radix wraps content in a display:table div that defeats `truncate`.
        viewportClassName="px-1.5 py-2 [&>div]:!block"
      >
        {hostOrder.map((id) => hosts[id] && <HostSection key={id} host={hosts[id]} />)}
      </ScrollArea>
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
        <Tooltip>
          <TooltipTrigger asChild>
            <span
              className={cn('size-1.5 rounded-full', STATE_DOT[host.state])}
              data-host-state={host.state}
              aria-label={hostStateLabel(host)}
            />
          </TooltipTrigger>
          <TooltipContent>{hostStateLabel(host)}</TooltipContent>
        </Tooltip>
        <div className="flex items-center opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
          <IconButton
            label="Add repository"
            className={cn('size-5 rounded', SIDEBAR_HOVER)}
            onClick={() => useStore.setState({ addRepoFor: host.hostId })}
          >
            <FolderPlus className="size-3.5" />
          </IconButton>
          <IconButton
            label="Disconnect"
            className={cn('size-5 rounded', SIDEBAR_HOVER)}
            onClick={() => void disconnectHost(host.hostId)}
          >
            <LogOut className="size-3.5" />
          </IconButton>
        </div>
      </div>
      {host.transport === 'stream' && (
        <div className="px-1.5 pb-1 text-[10px] text-muted-foreground">
          via SSH tunnel (UDP unavailable)
        </div>
      )}
      {(repos ?? []).map((repo) => (
        <RepoGroup key={repo.id} hostId={host.hostId} repoId={repo.id} name={repo.name} />
      ))}
      {repos && repos.length === 0 && (
        <Button
          type="button"
          variant="link"
          className="mx-1.5 mt-1 h-auto p-0 text-xs font-normal text-muted-foreground underline hover:text-foreground"
          onClick={() => useStore.setState({ addRepoFor: host.hostId })}
        >
          Add a repository…
        </Button>
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
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              className={cn(
                'h-5 gap-1 rounded px-1 text-[11px] font-medium normal-case tracking-normal text-muted-foreground opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100 hover:text-foreground has-[>svg]:px-1',
                SIDEBAR_HOVER
              )}
              onClick={() => useStore.setState({ newWorktree: { hostId, repo: repoId } })}
            >
              <Sparkles className="size-3" />
              New
            </Button>
          </TooltipTrigger>
          <TooltipContent>New worktree + launch agent</TooltipContent>
        </Tooltip>
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

function hostStateLabel(host: HostView): string {
  const transport = host.transport ? ` · ${host.transport === 'quic' ? 'QUIC' : 'SSH tunnel'}` : ''
  const rtt = host.rttMs != null ? ` · ${host.rttMs} ms` : ''
  return `${host.state}${transport}${rtt}`
}
