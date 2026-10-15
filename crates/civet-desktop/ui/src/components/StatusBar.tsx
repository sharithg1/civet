// 24px status bar (v3): active host + latency (click for the host popover),
// navigation hints, and the focused session on the right.
import { reconnectHost, requestRestartDaemon, upgradeSummary, useStore, wsKey } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { DOT_LABEL, agentName, duration, paneDot } from '@/lib/worktree-view'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
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
    <footer className="flex h-6 shrink-0 items-center gap-3.5 overflow-hidden border-t border-border bg-chrome px-3 font-mono text-[10.5px] whitespace-nowrap text-faint">
      {host && hostId && <HostPopover hostId={hostId} />}
      {!host && <span>no host</span>}
      <span className="hidden sm:inline">⌘K switch · ⌥` previous · ⌥↑↓ next</span>
      <span className="flex-1" />
      <span className="truncate">{right}</span>
    </footer>
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
    'h-[26px] cursor-pointer rounded-[5px] border border-border bg-transparent text-[11.5px] text-body hover:bg-white/10 disabled:cursor-default disabled:opacity-50'
  return (
    <Popover>
      <PopoverTrigger className="flex cursor-pointer items-center gap-1.5 outline-none hover:text-foreground">
        <HostStateDot host={host} />
        {host.label} · {hostMeta(host)}
        {host.upgrade && (
          <span className="text-status-wait" data-upgrade-hint>
            · civetd v{host.upgrade.availableVersion} available
          </span>
        )}
      </PopoverTrigger>
      <PopoverContent
        side="top"
        align="start"
        sideOffset={6}
        className="flex w-[280px] flex-col gap-2 px-3.5 py-3 text-[12px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <div className="font-mono text-foreground">
          {host.label} · {host.state}
        </div>
        <div className="leading-[1.5] text-muted-foreground">
          {host.hostId} · {transport}
          {host.rttMs != null ? ` · ${host.rttMs} ms` : ''}
          {host.daemonVersion ? ` · civetd ${host.daemonVersion}` : ''}
          <br />
          {agents} agent{agents === 1 ? '' : 's'} and {shells} shell{shells === 1 ? '' : 's'} running under civetd.
          Closing Civet detaches; nothing stops.
        </div>
        {host.upgrade && (
          <div className="rounded-[5px] border border-border px-2.5 py-2 leading-[1.5]">
            <div className="font-mono text-[11.5px] text-status-wait">{upgradeSummary(hostId)}</div>
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
          <button type="button" className={btn} onClick={() => useStore.setState({ addRepoFor: hostId })}>
            Add repository…
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
