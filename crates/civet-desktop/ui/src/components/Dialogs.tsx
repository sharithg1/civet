// Small dialogs: add project (shadcn Dialog) and confirm (shadcn
// AlertDialog). Both are driven by the store and hand focus back to the
// terminal when they close.
import { useRef, useState } from 'react'
import { isTauri } from '@tauri-apps/api/core'
import { addProject, useStore, type ConfirmRequest } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle
} from '@/components/ui/dialog'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle
} from '@/components/ui/alert-dialog'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectSeparator,
  SelectTrigger,
  SelectValue
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { FolderPathInput } from '@/components/FolderPathInput'

/** Keeps the last non-null value so content stays put during the close animation. */
function useLast<T>(value: T | null): T | null {
  const [last, setLast] = useState(value)
  if (value !== null && value !== last) setLast(value)
  return value ?? last
}

/** Select value for "New SSH host…" (never a host id: ids are hostnames or "local"). */
const NEW_HOST = '\u0000new-ssh-host'

/**
 * "Add project": a git repository or a plain folder on any host, This Mac included even when
 * its daemon is not running yet (it starts on submit). Opened from the
 * sidebar's "+" (⌘O), a host's menu (preselected) or the status bar.
 */
export function AddProjectDialog() {
  const req = useStore((s) => s.addProject)
  const shown = useLast(req)
  const close = () => useStore.setState({ addProject: null })
  return (
    <Dialog open={req !== null} onOpenChange={(open) => !open && close()}>
      <DialogContent onCloseAutoFocus={restoreTerminalFocus}>
        <DialogHeader className="gap-1.5">
          <DialogTitle>Add project</DialogTitle>
          <DialogDescription>
            A git repository or any folder on this Mac or a connected host.
          </DialogDescription>
        </DialogHeader>
        {shown && (
          <AddProjectForm key={shown.hostId ?? ''} initialHost={shown.hostId} onDone={close} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function defaultLocation(): string {
  const s = useStore.getState()
  return s.active?.hostId ?? s.hostOrder[0] ?? 'local'
}

function AddProjectForm({
  initialHost,
  onDone
}: {
  initialHost: string | null
  onDone: () => void
}) {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const canPick = useStore((s) => s.backend?.kind !== 'mock') && isTauri()
  const [hostId, setHostId] = useState(() => initialHost ?? defaultLocation())
  const [path, setPath] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // This Mac is always offered; it connects on submit.
  const locations = hostOrder.includes('local') ? hostOrder : ['local', ...hostOrder]
  const isLocal = hostId === 'local'
  const localUp = hosts.local?.state === 'connected'

  async function choose() {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const picked = await open({
        directory: true,
        multiple: false,
        title: 'Choose a repository or folder'
      })
      if (typeof picked === 'string') setPath(picked)
    } catch (err) {
      setError((err as Error).message ?? String(err))
    }
  }

  async function submit() {
    setBusy(true)
    setError(null)
    try {
      await addProject(hostId, path.trim())
      onDone()
    } catch (err) {
      setError((err as Error).message ?? String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="project-location" className="text-xs text-muted-foreground">
          Location
        </Label>
        <Select
          value={hostId}
          disabled={busy}
          onValueChange={(v) => {
            if (v === NEW_HOST) {
              useStore.setState({
                addProject: null,
                addProjectAfterConnect: true,
                showConnect: true
              })
              return
            }
            setHostId(v)
            setPath('')
            setError(null)
          }}
        >
          <SelectTrigger id="project-location" className="h-8 w-full text-sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {locations.map((id) => (
              <SelectItem key={id} value={id}>
                {id === 'local' ? 'This Mac' : (hosts[id]?.label ?? id)}
              </SelectItem>
            ))}
            <SelectSeparator />
            <SelectItem value={NEW_HOST}>New SSH host…</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="project-path" className="text-xs text-muted-foreground">
          Folder
        </Label>
        <div className="flex gap-2">
          <FolderPathInput
            id="project-path"
            hostId={hostId}
            className="h-8 min-w-0 flex-1 rounded-[5px] border-border bg-field font-mono text-sm shadow-none dark:bg-field"
            placeholder={
              isLocal ? '~/src/project' : `~/src/project on ${hosts[hostId]?.label ?? hostId}`
            }
            value={path}
            spellCheck={false}
            autoFocus
            disabled={busy}
            onValueChange={setPath}
          />
          {isLocal && canPick && (
            <Button
              type="button"
              variant="outline"
              size="md"
              disabled={busy}
              onClick={() => void choose()}
            >
              Choose…
            </Button>
          )}
        </div>
        <p className="text-2xs text-muted-foreground">
          A folder that is not a git repository has no worktrees or git; it becomes a repository
          once you run git init in it.
        </p>
      </div>
      {busy && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
          <Spinner className="size-3.5" />
          {isLocal && !localUp ? 'Starting Civet on this Mac…' : 'Adding project…'}
        </div>
      )}
      {error && !busy && <p className="text-xs text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" size="md" onClick={onDone}>
          Cancel
        </Button>
        <Button type="submit" size="md" disabled={busy || !path.trim()}>
          Add project
        </Button>
      </DialogFooter>
    </form>
  )
}

export function ConfirmDialog() {
  const req = useStore((s) => s.confirm)
  const shown: ConfirmRequest | null = useLast(req)
  const confirmRef = useRef<HTMLButtonElement>(null)
  const close = () => useStore.setState({ confirm: null })
  return (
    <AlertDialog open={req !== null} onOpenChange={(open) => !open && close()}>
      <AlertDialogContent
        onOpenAutoFocus={(e) => {
          // Enter confirms, as before (Radix would focus Cancel), unless the
          // request asks for the safer default.
          if (req?.focusCancel) return
          e.preventDefault()
          confirmRef.current?.focus()
        }}
        onCloseAutoFocus={restoreTerminalFocus}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>{shown?.title}</AlertDialogTitle>
          <AlertDialogDescription>{shown?.body}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel variant="outline" size="md">
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            ref={confirmRef}
            variant="destructive"
            size="md"
            onClick={() => {
              close()
              req?.onConfirm()
            }}
          >
            {shown?.confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
