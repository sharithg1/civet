// "New workspace" / "Edit roots": a floating workspace (state/floating.ts)
// is a name plus roots, each a path on the host typed with the same folder
// completion as Add project (a file's name is typed out; only folders are
// suggested). The host's projects are one click away as roots, since a task
// across repos is the usual reason to make one. Opened from the sidebar nav,
// a host's menu, or a workspace row's menu (roots only: the name and host
// are fixed once created).
import { useState } from 'react'
import { Plus, X } from 'lucide-react'
import {
  closeWorkspaceDialog,
  cleanRoots,
  createFloatingWorkspace,
  hasFloatingWorkspaces,
  setFloatingWorkspaceRoots,
  useWorkspaceDialog,
  WORKSPACES_MIN_DAEMON_VERSION,
  type WorkspaceDialogRequest
} from '@/state/floating'
import { isWorkspaceRepo, useStore } from '@/state/store'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue
} from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { FolderPathInput } from '@/components/FolderPathInput'

const FIELD =
  'h-8 min-w-0 flex-1 rounded-[5px] border-border bg-field text-sm shadow-none dark:bg-field'

export function WorkspaceDialog() {
  const req = useWorkspaceDialog((s) => s.request)
  // Keeps the last request so the content stays put during the close animation.
  const [last, setLast] = useState(req)
  if (req !== null && req !== last) setLast(req)
  const shown = req ?? last
  const editing = useStore((s) =>
    shown?.repo && shown.hostId
      ? s.repos[shown.hostId]?.find((r) => r.id === shown.repo && isWorkspaceRepo(r))
      : undefined
  )
  return (
    <Dialog open={req !== null} onOpenChange={(open) => !open && closeWorkspaceDialog()}>
      <DialogContent onCloseAutoFocus={restoreTerminalFocus}>
        <DialogHeader className="gap-1.5">
          <DialogTitle>{editing ? `Roots of ${editing.name}` : 'New workspace'}</DialogTitle>
          <DialogDescription>
            {editing
              ? 'New sessions see the change; agents already running keep the roots they started with.'
              : 'A place for a task that is not tied to one repository. Sessions start in a folder of its own that links to each root.'}
          </DialogDescription>
        </DialogHeader>
        {shown && (
          <WorkspaceForm
            key={`${shown.hostId ?? ''}:${shown.repo ?? ''}`}
            request={shown}
            initialRoots={editing?.roots?.map((r) => r.path)}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function defaultHost(): string {
  const s = useStore.getState()
  return s.active?.hostId ?? s.hostOrder[0] ?? 'local'
}

function WorkspaceForm({
  request,
  initialRoots
}: {
  request: WorkspaceDialogRequest
  initialRoots?: string[]
}) {
  const editing = request.repo
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const [hostId, setHostId] = useState(() => request.hostId ?? defaultHost())
  const [name, setName] = useState('')
  const [roots, setRoots] = useState<string[]>(() => (initialRoots?.length ? initialRoots : ['']))
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const projects = useStore((s) => s.repos[hostId])
  const locations = hostOrder.includes('local') ? hostOrder : ['local', ...hostOrder]
  const host = hosts[hostId]
  // This Mac may not be connected yet: its daemon starts on submit and is
  // this app's own version.
  const supported = hasFloatingWorkspaces(host) || (hostId === 'local' && !host?.daemonVersion)
  const given = cleanRoots(roots)
  const suggestions = (projects ?? []).filter((p) => !isWorkspaceRepo(p) && !given.includes(p.path))

  const setRoot = (i: number, value: string) =>
    setRoots((list) => list.map((r, j) => (j === i ? value : r)))
  const addRoot = (value = '') =>
    setRoots((list) => {
      const blank = list.findIndex((r) => !r.trim())
      // A suggestion fills the first empty row before adding one.
      if (value && blank >= 0) return list.map((r, j) => (j === blank ? value : r))
      return [...list, value]
    })

  async function submit() {
    setBusy(true)
    setError(null)
    try {
      if (editing) await setFloatingWorkspaceRoots(hostId, editing, roots)
      else await createFloatingWorkspace(hostId, name, roots)
      closeWorkspaceDialog()
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
      {!editing && (
        <>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="workspace-location" className="text-xs text-muted-foreground">
              Location
            </Label>
            <Select
              value={hostId}
              disabled={busy}
              onValueChange={(v) => {
                setHostId(v)
                setRoots([''])
                setError(null)
              }}
            >
              <SelectTrigger id="workspace-location" className="h-8 w-full text-sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {locations.map((id) => (
                  <SelectItem key={id} value={id}>
                    {id === 'local' ? 'This Mac' : (hosts[id]?.label ?? id)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="workspace-name" className="text-xs text-muted-foreground">
              Name
            </Label>
            <Input
              id="workspace-name"
              className={FIELD}
              placeholder="release notes"
              value={name}
              maxLength={64}
              spellCheck={false}
              autoFocus
              disabled={busy}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
        </>
      )}
      <div className="flex flex-col gap-1.5">
        <Label className="text-xs text-muted-foreground">Roots</Label>
        {roots.map((root, i) => (
          // Rows are positional and edited in place: the index is their key.
          <div key={i} className="flex gap-1.5">
            <FolderPathInput
              hostId={hostId}
              aria-label={`Root ${i + 1}`}
              className={`${FIELD} font-mono`}
              placeholder={i === 0 ? '~/src/project' : '~/notes/todo.md'}
              value={root}
              spellCheck={false}
              disabled={busy}
              onValueChange={(v) => setRoot(i, v)}
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove root ${i + 1}`}
              disabled={busy}
              onClick={() =>
                setRoots((list) => (list.length > 1 ? list.filter((_, j) => j !== i) : ['']))
              }
            >
              <X />
            </Button>
          </div>
        ))}
        <div className="flex flex-wrap items-center gap-1.5">
          <Button
            type="button"
            variant="outline"
            size="xs"
            disabled={busy}
            onClick={() => addRoot()}
          >
            <Plus />
            Add root
          </Button>
          {suggestions.map((p) => (
            <Button
              key={p.id}
              type="button"
              variant="ghost"
              size="xs"
              title={`Add ${p.path}`}
              aria-label={`Add project ${p.name} as a root`}
              disabled={busy}
              onClick={() => addRoot(p.path)}
              className="text-muted-foreground"
            >
              <Plus />
              {p.name}
            </Button>
          ))}
        </div>
        <p className="text-2xs text-muted-foreground">
          Repositories, worktrees, folders or single files on the host. Claude Code and Codex get
          each folder as --add-dir; nothing is copied or moved.
        </p>
      </div>
      {busy && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground" aria-live="polite">
          <Spinner className="size-3.5" />
          {editing ? 'Saving roots…' : 'Creating workspace…'}
        </div>
      )}
      {!supported && !busy && (
        <p className="text-xs text-status-wait">
          {host?.state === 'connected'
            ? `civetd ${host.daemonVersion} on ${host.label} has no workspaces; they need v${WORKSPACES_MIN_DAEMON_VERSION} or newer (restart the daemon to upgrade).`
            : `${host?.label ?? hostId} is not connected.`}
        </p>
      )}
      {error && !busy && <p className="text-xs text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" size="md" onClick={closeWorkspaceDialog}>
          Cancel
        </Button>
        <Button type="submit" size="md" disabled={busy || !supported || (!editing && !name.trim())}>
          {editing ? 'Save roots' : 'Create workspace'}
        </Button>
      </DialogFooter>
    </form>
  )
}
