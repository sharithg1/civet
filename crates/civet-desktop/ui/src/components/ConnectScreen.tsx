// Connect-to-host screen. Form layout follows Orca's
// src/renderer/src/components/sidebar/AddRemoteHostSshFormPanel.tsx and
// AddRemoteHostFields.tsx (SSH destination field); host config picker,
// identity files and the Orca-server/WSL variants were dropped. Bootstrap
// progress comes from `civet://bootstrap-progress`.

import { useState, type ReactNode } from 'react'
import { Laptop, Server } from 'lucide-react'
import { connectHost, reportError, useStore } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import type { BootstrapStep } from '@/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Spinner } from '@/components/ui/spinner'
import { Switch } from '@/components/ui/switch'

const STEP_LABEL: Record<BootstrapStep, string> = {
  probe: 'Checking the remote daemon',
  upload: 'Installing civetd',
  start: 'Starting civetd',
  resolve: 'Exchanging session key',
  dial: 'Opening QUIC connection',
  fallback: 'UDP unavailable, using SSH tunnel'
}

const INTRO = (
  <>
    Civet bootstraps <code className="font-mono text-xs">civetd</code> over SSH, then switches to
    QUIC so sessions survive sleep and network changes.
  </>
)

/**
 * Without `onCancel`: the full-window first-run screen. With it: a modal
 * dialog over the workspace ("Connect to another host"), closable with
 * Escape / Cancel unless a connection attempt is in flight.
 */
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

  const form = (header: ReactNode) => (
    <form
      className="flex flex-col gap-5"
      onSubmit={(e) => {
        e.preventDefault()
        void connect({ kind: 'ssh', destination })
      }}
    >
      {header}
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="ssh-destination" className="text-xs text-muted-foreground">
          SSH destination
        </Label>
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
          <p className="text-2xs text-muted-foreground">
            Mock backend: any name works; names containing “fail” fail, “udp-blocked” falls back to
            SSH.
          </p>
        )}
      </div>
      <div className="flex items-center justify-between gap-3">
        <div className="flex flex-col">
          <Label htmlFor="force-ssh" className="text-xs">
            Force SSH fallback
          </Label>
          <span className="text-2xs text-muted-foreground">
            Skip QUIC and tunnel through SSH (for hosts where UDP is blocked).
          </span>
        </div>
        <Switch
          id="force-ssh"
          checked={forceSsh}
          onCheckedChange={setForceSsh}
          disabled={busy !== null}
        />
      </div>
      {busy && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
          <Spinner className="size-3.5" />
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
          type="button"
          variant="outline"
          disabled={busy !== null}
          onClick={() => void connect({ kind: 'local' })}
          title="Run civetd on this machine"
        >
          <Laptop />
          This Mac
        </Button>
        {onCancel && (
          <Button
            type="button"
            variant="ghost"
            className="ml-auto"
            onClick={onCancel}
            disabled={busy !== null}
          >
            Cancel
          </Button>
        )}
      </div>
    </form>
  )

  if (onCancel) {
    return (
      <Dialog open onOpenChange={(open) => !open && busy === null && onCancel()}>
        <DialogContent
          showCloseButton={false}
          className="sm:max-w-md"
          onCloseAutoFocus={restoreTerminalFocus}
          // A click outside must not abandon an in-flight connection.
          onInteractOutside={(e) => busy !== null && e.preventDefault()}
        >
          {form(
            <div className="flex flex-col gap-1">
              <DialogTitle>Connect to a host</DialogTitle>
              <DialogDescription>{INTRO}</DialogDescription>
            </div>
          )}
        </DialogContent>
      </Dialog>
    )
  }

  return (
    <div className="flex h-full w-full items-center justify-center bg-background p-6">
      <div className="w-full max-w-md rounded-lg border border-border bg-card p-6 shadow-floating">
        {form(
          <div className="flex flex-col gap-1">
            <h1 className="text-lg font-semibold">Connect to a host</h1>
            <p className="text-sm text-muted-foreground">{INTRO}</p>
          </div>
        )}
      </div>
    </div>
  )
}
