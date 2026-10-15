// Small dialogs: add repository (shadcn Dialog) and confirm (shadcn
// AlertDialog). Both are driven by the store and hand focus back to the
// terminal when they close.
import { useRef, useState } from 'react'
import { addRepo, useStore, type ConfirmRequest } from '@/state/store'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

/** Keeps the last non-null value so content stays put during the close animation. */
function useLast<T>(value: T | null): T | null {
  const [last, setLast] = useState(value)
  if (value !== null && value !== last) setLast(value)
  return value ?? last
}

export function AddRepoDialog() {
  const hostId = useStore((s) => s.addRepoFor)
  const shownHost = useLast(hostId)
  const close = () => useStore.setState({ addRepoFor: null })
  return (
    <Dialog open={hostId !== null} onOpenChange={(open) => !open && close()}>
      <DialogContent onCloseAutoFocus={restoreTerminalFocus}>
        <DialogHeader className="gap-1.5">
          <DialogTitle className="text-[14px] leading-normal">Add repository</DialogTitle>
          <DialogDescription>A git repository on {shownHost}.</DialogDescription>
        </DialogHeader>
        {shownHost && <AddRepoForm hostId={shownHost} onDone={close} />}
      </DialogContent>
    </Dialog>
  )
}

function AddRepoForm({ hostId, onDone }: { hostId: string; onDone: () => void }) {
  const [path, setPath] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  async function submit() {
    setBusy(true)
    setError(null)
    try {
      await addRepo(hostId, path.trim())
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
        <Label htmlFor="repo-path" className="text-xs text-muted-foreground">
          Path on the host
        </Label>
        <Input
          id="repo-path"
          className="h-8 rounded-[5px] border-border bg-field font-mono text-[12.5px] shadow-none md:text-[12.5px] dark:bg-field"
          placeholder="~/src/project"
          value={path}
          spellCheck={false}
          onChange={(e) => setPath(e.target.value)}
        />
      </div>
      {error && <p className="text-xs text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" size="md" onClick={onDone}>
          Cancel
        </Button>
        <Button type="submit" size="md" disabled={busy || !path.trim()}>
          Add
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
          <AlertDialogTitle className="text-[14px] leading-normal">{shown?.title}</AlertDialogTitle>
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
