// "Remove <worktree>?" (v3): lists what would be lost before anything is
// deleted. civetd can't report uncommitted files or unpushed commits yet, so
// the first attempt is a plain `remove_worktree` (git refuses a dirty
// worktree) and only after that refusal does the button become
// "Remove anyway" (force). Live agent sessions are counted client-side.
// Confirming closes the dialog at once and the removal runs in the background
// (store `removing`): a modal left open while git works would block the whole
// app. A refusal arrives as a toast and is remembered (`removeRefused`), so
// reopening the dialog offers "Remove anyway". A locked worktree (`git
// worktree lock`) can only be removed with force, so it starts there.
import { panesOfWorktree, removeWorktree, useStore, wsKey } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { splitBranch, worktreeBranch } from '@/lib/worktree-view'
import { Button } from '@/components/ui/button'
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogTitle
} from '@/components/ui/alert-dialog'
import { useWorktreeInfo } from './chrome'

export function RemoveWorktreeDialog() {
  const req = useStore((s) => s.removeWorktree)
  return (
    <AlertDialog
      open={req !== null}
      onOpenChange={(o) => !o && useStore.setState({ removeWorktree: null })}
    >
      <AlertDialogContent
        className="w-[420px] max-w-[calc(100vw-32px)] gap-2.5 sm:max-w-[420px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        {req && (
          <Body key={`${req.hostId}:${req.worktree}`} hostId={req.hostId} worktree={req.worktree} />
        )}
      </AlertDialogContent>
    </AlertDialog>
  )
}

function Body({ hostId, worktree }: { hostId: string; worktree: string }) {
  const wt = useWorktreeInfo(hostId, worktree)
  const allPanes = useStore((s) => s.panes[hostId])
  const refused = useStore((s) => s.removeRefused[wsKey(hostId, worktree)] ?? null)
  const removing = useStore((s) => !!s.removing[wsKey(hostId, worktree)])
  const close = () => useStore.setState({ removeWorktree: null })
  if (!wt) return null
  const live = panesOfWorktree(allPanes, worktree).filter((p) => !p.exited)
  const busyAgents = live.filter(
    (p) => p.agent && (p.status === 'running' || p.status === 'needsInput')
  ).length
  // Plain consequences, warnings marked: no reassuring checkmarks.
  const branch = wt.info.branch
  const notes: { bad: boolean; text: string }[] = [
    {
      bad: false,
      text: `Deletes the folder ${wt.info.path}${branch ? `; the branch ${branch} is kept` : ''}.`
    }
  ]
  if (busyAgents) {
    notes.push({
      bad: true,
      text: `${busyAgents} agent session${busyAgents === 1 ? ' is' : 's are'} still working and will be stopped.`
    })
  } else if (live.length) {
    notes.push({
      bad: false,
      text: `Closes ${live.length} open session${live.length === 1 ? '' : 's'}.`
    })
  }
  notes.push(
    refused
      ? { bad: true, text: `Git refused: ${refused}` }
      : wt.info.locked
        ? { bad: true, text: 'Locked with git worktree lock.' }
        : { bad: false, text: 'Git refuses if there are uncommitted changes.' }
  )
  const locked = wt.info.locked
  const force = refused !== null || busyAgents > 0 || locked
  function run() {
    close()
    // Why: git refuses a locked worktree unless forced (twice; civetd does that).
    void removeWorktree(hostId, worktree, refused !== null || locked)
  }
  return (
    <>
      <AlertDialogTitle>Remove {splitBranch(worktreeBranch(wt.info)).name}?</AlertDialogTitle>
      <AlertDialogDescription className="sr-only">{wt.info.path}</AlertDialogDescription>
      {notes.map((c) => (
        <div
          key={c.text}
          className={c.bad ? 'flex gap-[9px] text-sm text-status-err-text' : 'text-sm text-soft'}
        >
          {c.bad && <span className="w-3 shrink-0 font-mono">!</span>}
          <span className="min-w-0 break-words">{c.text}</span>
        </div>
      ))}
      <div className="mt-1.5 flex justify-end gap-2">
        <Button type="button" variant="outline" size="md" onClick={close}>
          Cancel
        </Button>
        <Button type="button" size="md" variant="destructive" disabled={removing} onClick={run}>
          {force ? 'Remove anyway' : 'Remove worktree'}
        </Button>
      </div>
    </>
  )
}
