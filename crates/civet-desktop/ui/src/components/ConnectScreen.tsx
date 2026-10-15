// Connect-to-host screen. Form layout follows Orca's
// src/renderer/src/components/sidebar/AddRemoteHostSshFormPanel.tsx and
// AddRemoteHostFields.tsx (SSH destination field); host config picker,
// identity files and the Orca-server/WSL variants were dropped. Bootstrap
// progress comes from `civet://bootstrap-progress`.

import { useState } from 'react'
import { Laptop, Loader2, Server } from 'lucide-react'
import { connectHost, reportError, useStore } from '../state/store'
import type { BootstrapStep } from '../types'
import { Button, Input, Label, Switch } from './ui'

const STEP_LABEL: Record<BootstrapStep, string> = {
  probe: 'Checking the remote daemon',
  upload: 'Installing civetd',
  start: 'Starting civetd',
  resolve: 'Exchanging session key',
  dial: 'Opening QUIC connection',
  fallback: 'UDP unavailable, using SSH tunnel'
}

export function ConnectScreen({ onCancel }: { onCancel?: () => void }) {
  const [destination, setDestination] = useState('')
  const [forceSsh, setForceSsh] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const pending = useStore((s) => (busy ? s.hosts[busy] : undefined))
  const isMock = useStore((s) => s.backend?.kind === 'mock')

  async function connect(target: { kind: 'ssh'; destination: string } | { kind: 'local' }) {
    const id = target.kind === 'local' ? 'local' : target.destination.trim()
    if (!id) return
    setBusy(id)
    setError(null)
    try {
      await connectHost(target, forceSsh ? 'ssh' : 'auto')
    } catch (err) {
      const message = (err as Error).message ?? String(err)
      setError(message)
      reportError(`Could not connect to ${id}`, err)
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="flex h-full w-full items-center justify-center bg-background p-6">
      <form
        className="flex w-full max-w-md flex-col gap-5 rounded-lg border border-border bg-card p-6 shadow-floating"
        onSubmit={(e) => {
          e.preventDefault()
          void connect({ kind: 'ssh', destination })
        }}
      >
        <div className="flex flex-col gap-1">
          <h1 className="text-lg font-semibold">Connect to a host</h1>
          <p className="text-sm text-muted-foreground">
            Civet bootstraps <code className="font-mono text-xs">civetd</code> over SSH, then switches
            to QUIC so sessions survive sleep and network changes.
          </p>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="ssh-destination">SSH destination</Label>
          <Input
            id="ssh-destination"
            placeholder="user@devbox or a Host from ~/.ssh/config"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            value={destination}
            disabled={busy !== null}
            onChange={(e) => setDestination(e.target.value)}
          />
          {isMock && (
            <p className="text-[11px] text-muted-foreground">
              Mock backend: any name works; names containing “fail” fail, “udp-blocked” falls back to SSH.
            </p>
          )}
        </div>
        <div className="flex items-center justify-between gap-3">
          <div className="flex flex-col">
            <Label htmlFor="force-ssh" className="text-foreground">
              Force SSH fallback
            </Label>
            <span className="text-[11px] text-muted-foreground">
              Skip QUIC and tunnel through SSH (for hosts where UDP is blocked).
            </span>
          </div>
          <Switch id="force-ssh" checked={forceSsh} onChange={setForceSsh} disabled={busy !== null} />
        </div>
        {busy && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
            <Loader2 className="size-3.5 animate-spin" />
            {pending?.bootstrap
              ? `${STEP_LABEL[pending.bootstrap.step]}${pending.bootstrap.message ? ` — ${pending.bootstrap.message}` : ''}`
              : `Connecting to ${busy}…`}
          </div>
        )}
        {error && !busy && <p className="text-xs text-destructive">{error}</p>}
        <div className="flex items-center gap-2">
          <Button type="submit" disabled={busy !== null || destination.trim() === ''}>
            <Server />
            Connect
          </Button>
          <Button
            variant="outline"
            disabled={busy !== null}
            onClick={() => void connect({ kind: 'local' })}
            title="Run civetd on this machine"
          >
            <Laptop />
            This Mac
          </Button>
          {onCancel && (
            <Button variant="ghost" className="ml-auto" onClick={onCancel} disabled={busy !== null}>
              Cancel
            </Button>
          )}
        </div>
      </form>
    </div>
  )
}
