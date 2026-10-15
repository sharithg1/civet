// Connection line above the terminal, driven only by `civet://host-state`.
// The Rust core owns the 1 s threshold (INTERFACES §6.3): sub-second resumes
// never emit "reconnecting", so this shows exactly what the backend reports.
// v3 look: one quiet mono line; failed / disconnected keep a Reconnect action.
import { connectHost, reportError, useStore } from '@/state/store'

export function ConnectionBanner({ hostId }: { hostId: string }) {
  const host = useStore((s) => s.hosts[hostId])
  if (!host || host.state === 'connected' || host.state === 'connecting') return null
  if (host.state === 'reconnecting') {
    return (
      <div
        role="status"
        className="shrink-0 border-b border-border px-[18px] py-1.5 font-mono text-[11.5px] text-muted-foreground"
      >
        reconnecting to {host.label}… sessions keep running on the host
      </div>
    )
  }
  const retry = () => {
    const target =
      hostId === 'local'
        ? ({ kind: 'local' } as const)
        : ({ kind: 'ssh', destination: hostId } as const)
    connectHost(target, host.transport === 'stream' ? 'ssh' : 'auto').catch((err) =>
      reportError(`Could not reconnect to ${host.label}`, err)
    )
  }
  return (
    <div
      role="alert"
      className="flex shrink-0 items-center gap-2 border-b border-border px-[18px] py-1 font-mono text-[11.5px] text-status-err-text"
    >
      <span className="min-w-0 truncate">
        {host.state === 'failed' ? `connection to ${host.label} failed` : `disconnected from ${host.label}`}
        {host.message && <span className="text-faint"> · {host.message}</span>}
      </span>
      <button
        type="button"
        onClick={retry}
        className="ml-auto h-[22px] shrink-0 cursor-pointer rounded-[5px] border border-border px-2 font-sans text-[11.5px] text-soft hover:bg-hover-strong"
      >
        Reconnect
      </button>
    </div>
  )
}
