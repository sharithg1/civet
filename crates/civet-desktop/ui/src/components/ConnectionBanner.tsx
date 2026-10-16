// Connection line above the terminal, driven only by `civet://host-state`.
// The Rust core owns the 1 s threshold (INTERFACES §6.3): sub-second resumes
// never emit "reconnecting", so this shows exactly what the backend reports.
// v3 look: one quiet mono line; failed / disconnected keep a Reconnect action
// (`reconnect_host`: Rust reuses the stored ssh target and transport). A host
// that kept an older civetd (live panes) shows the upgrade line with
// "Restart daemon", which asks before closing the panes.
import {
  livePaneCount,
  reconnectHost,
  requestRestartDaemon,
  upgradeSummary,
  useStore
} from '@/state/store'

const actionBtn =
  'h-[22px] shrink-0 cursor-pointer rounded-[5px] border border-border px-2 font-sans text-xs text-soft hover:bg-hover-strong'

export function ConnectionBanner({ hostId }: { hostId: string }) {
  const host = useStore((s) => s.hosts[hostId])
  const panes = useStore((s) => s.panes[hostId])
  const dismissed = useStore((s) => s.dismissedUpgrades[hostId])
  if (!host || host.state === 'connecting') return null
  if (host.state === 'connected') {
    const u = host.upgrade
    if (!u || dismissed === `${u.runningVersion}>${u.availableVersion}`) return null
    // `panes` keeps this line live as panes open and close.
    const n = panes ? Object.keys(panes).length : livePaneCount(hostId)
    return (
      <div
        role="status"
        data-upgrade-banner
        className="flex shrink-0 items-center gap-2 border-b border-border px-[18px] py-1 font-mono text-xs text-status-wait"
      >
        <span className="min-w-0 truncate">
          {upgradeSummary(hostId)}
          <span className="text-faint"> · restart to upgrade</span>
        </span>
        <span className="flex-1" />
        <button type="button" onClick={() => requestRestartDaemon(hostId)} className={actionBtn}>
          Restart daemon (closes {n} pane{n === 1 ? '' : 's'})
        </button>
        <button
          type="button"
          onClick={() =>
            useStore.setState((s) => ({
              dismissedUpgrades: {
                ...s.dismissedUpgrades,
                [hostId]: `${u.runningVersion}>${u.availableVersion}`
              }
            }))
          }
          className="h-[22px] shrink-0 cursor-pointer px-1.5 font-sans text-xs text-faint hover:text-foreground"
        >
          Later
        </button>
      </div>
    )
  }
  if (host.state === 'reconnecting') {
    return (
      <div
        role="status"
        className="shrink-0 border-b border-border px-[18px] py-1.5 font-mono text-xs text-muted-foreground"
      >
        reconnecting to {host.label}… sessions keep running on the host
      </div>
    )
  }
  return (
    <div
      role="alert"
      className="flex shrink-0 items-center gap-2 border-b border-border px-[18px] py-1 font-mono text-xs text-status-err-text"
    >
      <span className="min-w-0 truncate" title={host.message ?? undefined}>
        {host.state === 'failed'
          ? `connection to ${host.label} failed`
          : `disconnected from ${host.label}`}
        {host.message && <span className="text-faint"> · {host.message}</span>}
      </span>
      <span className="flex-1" />
      {host.errorCode === 'daemonBusy' && (
        <button type="button" onClick={() => requestRestartDaemon(hostId)} className={actionBtn}>
          Restart daemon…
        </button>
      )}
      <button type="button" onClick={() => void reconnectHost(hostId)} className={actionBtn}>
        Reconnect
      </button>
    </div>
  )
}
