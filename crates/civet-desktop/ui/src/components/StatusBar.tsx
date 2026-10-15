// Footer of the main column, like the line under Cursor's composer: the
// worktree's branch, the active host + latency (click for the host popover)
// and, on the right, the focused session.
import { ChevronDown, GitBranch, Server } from 'lucide-react'
import { reconnectHost, requestRestartDaemon, upgradeSummary, useStore, wsKey } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { DOT_LABEL, agentName, duration, paneDot, worktreeBranch } from '@/lib/worktree-view'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { useWorktreeInfo } from './chrome'
import { HostStateDot, hostMeta } from './Sidebar'

export function StatusBar() {
  const active = useStore((s) => s.active)
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const hostId = active?.hostId ?? hostOrder[0]
  const host = hostId ? hosts[hostId] : undefined
  const ws = useStore((s) => (active ? s.workspaces[wsKey(active.hostId, active.worktree)] : undefined))
  const focusedId = ws?.tabs.find((t) => t.id === ws.activeTab)?.focused
  const pane = useStore((s) => (active && focusedId !== undefined ? s.panes[active.hostId]?.[focusedId] : undefined))
  const connectedCount = hostOrder.filter((h) => hosts[h]?.state === 'connected').length
  let right = `${connectedCount} host${connectedCount === 1 ? '' : 's'} connected`
  if (pane?.agent) {
    right = `${agentName(pane)} · ${DOT_LABEL[paneDot(pane, host?.state === 'connected')].toLowerCase()} · ${duration(pane.createdAtMs)}`
  } else if (pane) {
    right = `${pane.command.join(' ')} · ${duration(pane.createdAtMs)}`
  }
  return (
    <footer className="flex h-8 shrink-0 items-center gap-1 overflow-hidden bg-background px-2 text-sm whitespace-nowrap text-quiet">
      {active && <BranchChip hostId={active.hostId} worktree={active.worktree} />}
      {host && hostId && <HostPopover hostId={hostId} />}
      {!host && <span className="px-1.5">no host</span>}
      <span className="flex-1" />
      <span className="truncate px-1.5 text-dim">{right}</span>
    </footer>
  )
}

function BranchChip({ hostId, worktree }: { hostId: string; worktree: string }) {
  const wt = useWorktreeInfo(hostId, worktree)
  if (!wt) return null
  return (
    <span className="flex h-6 min-w-0 items-center gap-1.5 px-1.5" title={wt.info.path}>
      <GitBranch className="size-3.5 shrink-0" />
      <span className="truncate font-mono text-xs text-soft">{worktreeBranch(wt.info)}</span>
    </span>
  )
}

function HostPopover({ hostId }: { hostId: string }) {
  const host = useStore((s) => s.hosts[hostId])!
  const backend = useStore((s) => s.backend)
  const panes = useStore((s) => s.panes[hostId])
  const live = Object.values(panes ?? {}).filter((p) => !p.exited)
  // Every pane the daemon holds (exited ones too) closes on a restart.
  const paneCount = panes ? Object.keys(panes).length : (host.upgrade?.livePanes ?? 0)
  const agents = live.filter((p) => p.agent).length
  const shells = live.length - agents
  const transport = host.transport === 'quic' ? 'QUIC' : host.transport === 'stream' ? 'SSH tunnel' : 'no transport'
  const mock = backend && 'simulateNetworkDrop' in backend ? (backend as unknown as { simulateNetworkDrop(h: string): void }) : null
  const btn =
    'h-7 cursor-pointer rounded-md border border-input bg-card text-sm text-soft hover:bg-hover-strong hover:text-foreground disabled:cursor-default disabled:opacity-50'
  return (
    <Popover>
      <PopoverTrigger className="flex h-6 cursor-pointer items-center gap-1.5 rounded-md px-1.5 outline-none hover:bg-hover hover:text-foreground data-[state=open]:bg-hover">
        <Server className="size-3.5 shrink-0" />
        {host.label}
        <HostStateDot host={host} />
        <span className="text-xs text-dim">{hostMeta(host)}</span>
        {host.upgrade && (
          <span className="text-status-wait" data-upgrade-hint>
            · civetd v{host.upgrade.availableVersion} available
          </span>
        )}
        <ChevronDown className="size-3 shrink-0" />
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="start"
        sideOffset={6}
        className="flex w-[280px] flex-col gap-2 px-3.5 py-3 text-xs"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <div className="font-mono text-foreground">
          {host.label} · {host.state}
        </div>
        <div className="leading-normal text-muted-foreground">
          {host.hostId} · {transport}
          {host.rttMs != null ? ` · ${host.rttMs} ms` : ''}
          {host.daemonVersion ? ` · civetd ${host.daemonVersion}` : ''}
          <br />
          {agents} agent{agents === 1 ? '' : 's'} and {shells} shell{shells === 1 ? '' : 's'} running under civetd.
          Closing Civet detaches; nothing stops.
        </div>
        {host.upgrade && (
          <div className="rounded-[5px] border border-border px-2.5 py-2 leading-normal">
            <div className="font-mono text-xs text-status-wait">{upgradeSummary(hostId)}</div>
            <div className="mt-1 text-muted-foreground">
              The older daemon keeps running so your sessions survive. Restarting upgrades it and closes them.
            </div>
            <button
              type="button"
              className={`${btn} mt-2 w-full`}
              onClick={() => requestRestartDaemon(hostId)}
            >
              Restart daemon (closes {paneCount} pane{paneCount === 1 ? '' : 's'})
            </button>
          </div>
        )}
        {(host.state === 'failed' || host.state === 'disconnected') && (
          <button type="button" className={btn} onClick={() => void reconnectHost(hostId)}>
            Reconnect
          </button>
        )}
        <div className="grid grid-cols-2 gap-1.5">
          <button type="button" className={btn} onClick={() => useStore.setState({ addProject: { hostId } })}>
            Add project…
          </button>
          <button type="button" className={btn} onClick={() => useStore.setState({ showConnect: true })}>
            Connect host…
          </button>
        </div>
        {mock && (
          <button
            type="button"
            className={btn}
            disabled={host.state !== 'connected'}
            onClick={() => mock.simulateNetworkDrop(hostId)}
          >
            Simulate network drop (mock)
          </button>
        )}
      </PopoverContent>
    </Popover>
  )
}
