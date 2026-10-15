// "Reconnecting…" banner, driven only by `civet://host-state`. The Rust core
// owns the 1 s threshold (INTERFACES §6.3): sub-second resumes never emit
// "reconnecting", so this banner shows exactly what the backend reports.
// Styling follows Orca's status-bar/banner tokens (status-warning).
import { Loader2, RotateCw, WifiOff } from 'lucide-react'
import { connectHost, reportError, useStore } from '../state/store'
import { Button } from './ui'

export function ConnectionBanner({ hostId }: { hostId: string }) {
  const host = useStore((s) => s.hosts[hostId])
  if (!host || host.state === 'connected' || host.state === 'connecting') return null
  if (host.state === 'reconnecting') {
    return (
      <div
        role="status"
        className="flex h-7 shrink-0 items-center gap-2 border-b border-amber-500/30 bg-amber-500/12 px-3 text-xs text-amber-600 dark:text-amber-300"
      >
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        Reconnecting to {host.label}…
        <span className="text-muted-foreground">Your sessions keep running on the host.</span>
      </div>
    )
  }
  const retry = () => {
    const target =
      hostId === 'local' ? ({ kind: 'local' } as const) : ({ kind: 'ssh', destination: hostId } as const)
    connectHost(target, host.transport === 'stream' ? 'ssh' : 'auto').catch((err) =>
      reportError(`Could not reconnect to ${host.label}`, err)
    )
  }
  return (
    <div
      role="alert"
      className="flex h-8 shrink-0 items-center gap-2 border-b border-destructive/30 bg-destructive/10 px-3 text-xs text-destructive"
    >
      <WifiOff className="size-3.5" aria-hidden />
      {host.state === 'failed' ? `Connection to ${host.label} failed` : `Disconnected from ${host.label}`}
      {host.message && <span className="truncate text-muted-foreground">— {host.message}</span>}
      <Button size="xs" variant="outline" className="ml-auto" onClick={retry}>
        <RotateCw />
        Reconnect
      </Button>
    </div>
  )
}
