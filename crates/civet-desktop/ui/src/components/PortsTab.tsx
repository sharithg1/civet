// Ports tab of the right panel: the host's listening TCP ports (from the
// daemon) and port forwards to this Mac (one QUIC stream per forwarded
// connection, so they survive sleep and network changes like the panes).
//
// Forwards are host-wide and always listed; detected ports are split into
// this worktree's (opened by one of its panes, or running in its directory)
// and the rest of the host's. On This Mac ports are already local, so the
// tab only lists them with an Open button.
import { useState } from 'react'
import { ChevronRight, ExternalLink, Pin, Plus, X } from 'lucide-react'
import type { ForwardView, ListeningPort, PaneInfo } from '@/types'
import { pushToast, revealPane, useStore } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import {
  forwardPort,
  isPinned,
  localUrl,
  openLocalPort,
  reachableFromLoopback,
  stopForward,
  togglePin,
  usePorts
} from '@/state/ports'
import { cn } from '@/lib/utils'
import { IconButton } from './IconButton'
import { Checkbox } from './ui/checkbox'

export function PortsTab({
  hostId,
  worktree,
  panes,
  connected
}: {
  hostId: string
  worktree: string
  panes: PaneInfo[]
  connected: boolean
}) {
  const report = usePorts((s) => s.ports[hostId])
  const forwards = usePorts((s) => s.forwards[hostId]) ?? []
  const pins = usePrefs((s) => s.pinnedForwards)
  const hostPanes = useStore((s) => s.panes[hostId])
  const [showElsewhere, setShowElsewhere] = useState(false)
  const isLocal = hostId === 'local'

  const ports = report?.ports ?? []
  const byPort = new Map(ports.map((p) => [p.port, p]))
  const forwarded = new Set(forwards.map((f) => f.remotePort))
  const here = ports.filter((p) => p.worktree === worktree && !forwarded.has(p.port))
  const elsewhere = ports.filter((p) => p.worktree !== worktree && !forwarded.has(p.port))
  const paneOf = (id: number | null) =>
    id == null ? undefined : (panes.find((p) => p.id === id) ?? hostPanes?.[id])
  const reveal = (p: ListeningPort) => {
    if (p.pane != null) revealPane(hostId, p.worktree, p.pane)
  }

  if (report?.status === 'unsupported') {
    return (
      <div className="flex flex-col gap-2.5 px-2 py-0.5 text-sm">
        <Note>{report.message ?? 'Ports aren’t available on this host.'}</Note>
      </div>
    )
  }

  const detectedRow = (p: ListeningPort, showWorktree: boolean) => (
    <PortRow
      key={p.port}
      port={p.port}
      info={p}
      pane={paneOf(p.pane)}
      worktreeLabel={showWorktree ? worktreeName(p.worktree) : null}
      onPane={() => reveal(p)}
      sub={
        isLocal ? (
          <AddressButton port={p.port} />
        ) : (
          <span className="text-faint">not forwarded</span>
        )
      }
    >
      {isLocal ? (
        <IconButton label={`Open ${localUrl(p.port)}`} onClick={() => void openLocalPort(p.port)}>
          <ExternalLink />
        </IconButton>
      ) : (
        <SmallButton disabled={!connected} onClick={() => void forwardPort(hostId, p.port)}>
          Forward
        </SmallButton>
      )}
    </PortRow>
  )

  return (
    <div className="flex flex-col gap-2.5 px-2 py-0.5 text-sm">
      {isLocal && <Note>Ports on This Mac are already reachable at localhost.</Note>}
      {!report || report.status === 'pending' ? (
        <div className="text-xs text-faint">
          {connected ? 'Looking for listening ports…' : 'Not connected.'}
        </div>
      ) : null}

      {!isLocal && (
        <Group title="Forwarded" count={forwards.length}>
          {forwards.length === 0 && (
            <div className="text-xs text-muted-foreground">
              Nothing forwarded. Forward a detected port below, or add one by number.
            </div>
          )}
          {forwards.map((f) => {
            const info = byPort.get(f.remotePort)
            const pinned = isPinned(pins, hostId, f.remotePort)
            return (
              <PortRow
                key={f.remotePort}
                port={f.remotePort}
                info={info}
                live={!!info}
                pane={paneOf(info?.pane ?? null)}
                worktreeLabel={
                  info && info.worktree !== worktree ? worktreeName(info.worktree) : null
                }
                onPane={() => info && reveal(info)}
                sub={<ForwardSub f={f} listening={!!info || report?.status !== 'watching'} />}
                error={f.lastError}
              >
                <IconButton
                  label={`Open ${localUrl(f.localPort)}`}
                  onClick={() => void openLocalPort(f.localPort)}
                >
                  <ExternalLink />
                </IconButton>
                <IconButton
                  label={pinned ? 'Unpin (stop restoring on connect)' : 'Pin (restore on connect)'}
                  onClick={() => togglePin(hostId, f)}
                  className={pinned ? 'text-foreground' : undefined}
                >
                  <Pin className={pinned ? 'fill-current' : undefined} />
                </IconButton>
                <IconButton
                  label="Stop forwarding"
                  onClick={() => void stopForward(hostId, f.remotePort)}
                >
                  <X />
                </IconButton>
              </PortRow>
            )
          })}
        </Group>
      )}

      {report?.status === 'watching' && (
        <Group title="This worktree" count={here.length}>
          {here.length === 0 && (
            <div className="text-xs text-muted-foreground">
              Nothing in this worktree is listening.
            </div>
          )}
          {here.map((p) => detectedRow(p, false))}
        </Group>
      )}

      {report?.status === 'watching' && elsewhere.length > 0 && (
        <div className="flex flex-col gap-2 border-t border-border pt-2.5">
          <button
            type="button"
            onClick={() => setShowElsewhere((v) => !v)}
            className="flex cursor-pointer items-center gap-1 text-left text-xs text-faint hover:text-foreground"
          >
            <ChevronRight
              className={cn('size-3 transition-transform', showElsewhere && 'rotate-90')}
            />
            Elsewhere on host <span className="text-dim">· {elsewhere.length}</span>
          </button>
          {showElsewhere && elsewhere.map((p) => detectedRow(p, true))}
        </div>
      )}

      {!isLocal && (
        <AddForward
          connected={connected}
          existing={forwards.map((f) => f.remotePort)}
          onAdd={(remote, local, pin) => forwardPort(hostId, remote, local, { pin })}
        />
      )}
    </div>
  )
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded-[5px] border border-dashed border-border px-2 py-1.5 text-xs leading-normal text-faint">
      {children}
    </div>
  )
}

function Group({
  title,
  count,
  children
}: {
  title: string
  count: number
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-2 border-t border-border pt-2.5">
      <div className="text-xs text-faint">
        {title} <span className="text-dim">· {count}</span>
      </div>
      {children}
    </div>
  )
}

function SmallButton(props: React.ComponentProps<'button'>) {
  return (
    <button
      type="button"
      {...props}
      className="h-[22px] cursor-pointer rounded-[5px] border border-border px-2 text-xs text-soft hover:bg-hover disabled:cursor-default disabled:opacity-50"
    />
  )
}

function worktreeName(id: string | null): string | null {
  if (!id) return null
  return id.replace(/\/+$/, '').split('/').pop() || id
}

function AddressButton({ port, struck }: { port: number; struck?: boolean }) {
  const url = localUrl(port)
  return (
    <button
      type="button"
      title="Copy URL"
      onClick={() => {
        void navigator.clipboard?.writeText(url).catch(() => {})
        pushToast({ kind: 'info', title: `Copied ${url}` }, 3200)
      }}
      className={cn(
        'cursor-copy font-mono',
        struck ? 'text-faint line-through' : 'text-soft hover:text-foreground'
      )}
    >
      → localhost:{port}
    </button>
  )
}

function ForwardSub({ f, listening }: { f: ForwardView; listening: boolean }) {
  return (
    <>
      <AddressButton port={f.localPort} />
      {f.localPort !== f.remotePort && (
        <span className="text-faint" title={`localhost:${f.remotePort} was in use on this Mac`}>
          ({f.remotePort} taken)
        </span>
      )}
      {f.connections > 0 && (
        <span className="text-faint">
          · {f.connections} conn{f.connections === 1 ? '' : 's'}
        </span>
      )}
      {!listening && <span className="text-faint">· not listening</span>}
    </>
  )
}

function PortRow({
  port,
  info,
  live,
  pane,
  worktreeLabel,
  onPane,
  sub,
  error,
  children
}: {
  port: number
  info: ListeningPort | undefined
  /** Forwarded and the host reports something listening (green dot). */
  live?: boolean
  pane: PaneInfo | undefined
  worktreeLabel: string | null
  onPane: () => void
  sub: React.ReactNode
  /** Why the last forwarded connection failed. */
  error?: string | null
  children: React.ReactNode
}) {
  const unreachable = info && !reachableFromLoopback(info.addrs)
  return (
    <div className="group flex flex-col gap-0.5">
      <div className="flex items-center gap-2" title={info?.command ?? undefined}>
        <span
          aria-hidden
          className={cn(
            'inline-block size-[7px] shrink-0 rounded-full',
            live ? 'bg-status-ok' : 'border-[1.5px] border-dashed border-dim'
          )}
        />
        <span className="font-mono text-xs text-foreground">{port}</span>
        <span className="min-w-0 truncate text-muted-foreground">{info?.process ?? '—'}</span>
        <span className="ml-auto flex shrink-0 items-center gap-0.5">{children}</span>
      </div>
      <div className="flex min-w-0 items-center gap-1.5 pl-[15px] text-2xs">
        {sub}
        {(pane || worktreeLabel) && (
          <button
            type="button"
            onClick={onPane}
            disabled={!pane}
            className="ml-auto min-w-0 cursor-pointer truncate text-faint enabled:hover:text-foreground disabled:cursor-default"
            title={pane ? 'Show the pane that opened this port' : undefined}
          >
            {pane ? `${pane.agent ?? 'shell'} · pane ${pane.id}` : worktreeLabel}
          </button>
        )}
      </div>
      {unreachable && (
        <div className="pl-[15px] text-2xs text-faint">
          Listens on {info.addrs.join(', ')} only; forwarding reaches loopback.
        </div>
      )}
      {error && <div className="pl-[15px] text-2xs text-status-err">{error}</div>}
    </div>
  )
}

function AddForward({
  connected,
  onAdd,
  existing
}: {
  connected: boolean
  onAdd: (remote: number, local: number | null, pin: boolean) => Promise<unknown>
  existing: number[]
}) {
  const [open, setOpen] = useState(false)
  const [remote, setRemote] = useState('')
  const [local, setLocal] = useState('')
  const [pinned, setPinned] = useState(false)
  const [busy, setBusy] = useState(false)
  const r = parsePort(remote)
  const l = local.trim() === '' ? null : parsePort(local)
  const valid = r != null && l !== undefined && !existing.includes(r)

  if (!open) {
    return (
      <button
        type="button"
        disabled={!connected}
        onClick={() => setOpen(true)}
        className="flex h-[26px] cursor-pointer items-center gap-1.5 self-start rounded-[5px] border border-border px-2.5 text-xs text-soft hover:bg-hover disabled:cursor-default disabled:opacity-50"
      >
        <Plus className="size-3" />
        Forward a port…
      </button>
    )
  }

  const submit = async () => {
    if (r == null || l === undefined || existing.includes(r) || busy) return
    setBusy(true)
    const ok = await onAdd(r, l, pinned)
    setBusy(false)
    if (!ok) return
    setRemote('')
    setLocal('')
    setPinned(false)
    setOpen(false)
  }

  return (
    <form
      className="flex flex-col gap-2 border-t border-border pt-2.5"
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      <div className="flex items-end gap-2">
        <PortField
          label="Remote"
          value={remote}
          onChange={setRemote}
          placeholder="8080"
          autoFocus
        />
        <span className="pb-1 text-faint">→</span>
        <PortField label="Local" value={local} onChange={setLocal} placeholder="auto" />
      </div>
      <label className="flex cursor-pointer items-center gap-2 text-xs text-muted-foreground">
        <Checkbox
          checked={pinned}
          onCheckedChange={(v) => setPinned(v === true)}
          className="size-3.5"
        />
        Restore on connect
      </label>
      {r != null && existing.includes(r) && (
        <div className="text-xs text-faint">{r} is already forwarded.</div>
      )}
      <div className="flex gap-1.5">
        <button
          type="submit"
          disabled={!valid || busy || !connected}
          className="h-[24px] cursor-pointer rounded-[5px] border border-border bg-hover px-2.5 text-xs text-foreground disabled:cursor-default disabled:opacity-50"
        >
          Forward
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          className="h-[24px] cursor-pointer rounded-[5px] px-2.5 text-xs text-muted-foreground hover:bg-hover"
        >
          Cancel
        </button>
      </div>
    </form>
  )
}

function PortField(props: {
  label: string
  value: string
  onChange: (v: string) => void
  placeholder: string
  autoFocus?: boolean
}) {
  return (
    <label className="flex min-w-0 flex-1 flex-col gap-1 text-2xs text-faint">
      {props.label}
      <input
        inputMode="numeric"
        value={props.value}
        autoFocus={props.autoFocus}
        placeholder={props.placeholder}
        onChange={(e) => props.onChange(e.target.value.replace(/\D/g, '').slice(0, 5))}
        className="h-[24px] w-full rounded-[5px] border border-border bg-transparent px-2 font-mono text-xs text-foreground outline-none placeholder:text-dim focus-visible:border-ring"
      />
    </label>
  )
}

/** A TCP port, or undefined when the text is not one. */
function parsePort(s: string): number | undefined {
  const n = Number(s)
  return s !== '' && Number.isInteger(n) && n >= 1 && n <= 65535 ? n : undefined
}
