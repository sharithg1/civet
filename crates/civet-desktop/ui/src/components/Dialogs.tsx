// Small dialogs: add repository and confirm. Built on ui.tsx (Orca dialog styling).
import { useState } from 'react'
import { addRepo, useStore } from '../state/store'
import { Button, Dialog, Input, Label } from './ui'

export function AddRepoDialog() {
  const hostId = useStore((s) => s.addRepoFor)
  const [path, setPath] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  if (!hostId) return null
  const close = () => useStore.setState({ addRepoFor: null })
  async function submit() {
    if (!hostId) return
    setBusy(true)
    setError(null)
    try {
      await addRepo(hostId, path.trim())
      close()
    } catch (err) {
      setError((err as Error).message ?? String(err))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog title="Add repository" description={`A git repository on ${hostId}.`} onClose={close}>
      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault()
          void submit()
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="repo-path">Path on the host</Label>
          <Input
            id="repo-path"
            className="font-mono"
            placeholder="~/src/project"
            value={path}
            spellCheck={false}
            onChange={(e) => setPath(e.target.value)}
          />
        </div>
        {error && <p className="text-xs text-destructive">{error}</p>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !path.trim()}>
            Add
          </Button>
        </div>
      </form>
    </Dialog>
  )
}

export function ConfirmDialog() {
  const req = useStore((s) => s.confirm)
  if (!req) return null
  const close = () => useStore.setState({ confirm: null })
  return (
    <Dialog title={req.title} description={req.body} onClose={close}>
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
        <Button
          variant="destructive"
          data-autofocus
          onClick={() => {
            close()
            req.onConfirm()
          }}
        >
          {req.confirmLabel}
        </Button>
      </div>
    </Dialog>
  )
}
