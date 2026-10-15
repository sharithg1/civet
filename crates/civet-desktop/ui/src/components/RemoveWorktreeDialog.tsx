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
    <AlertDialog open={req !== null} onOpenChange={(o) => !o && useStore.setState({ removeWorktree: null })}>
      <AlertDialogContent
        className="w-[420px] max-w-[calc(100vw-32px)] gap-2.5 sm:max-w-[420px]"
        onCloseAutoFocus={restoreTerminalFocus}
      >
        {req && <Body key={`${req.hostId}:${req.worktree}`} hostId={req.hostId} worktree={req.worktree} />}
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
  const busyAgents = live.filter((p) => p.agent && (p.status === 'running' || p.status === 'needsInput')).length
  const checks: { bad: boolean; text: string }[] = [
    busyAgents
      ? { bad: true, text: `${busyAgents} agent session${busyAgents === 1 ? '' : 's'} still active` }
      : { bad: false, text: 'No agent is working here' },
    live.length
      ? { bad: false, text: `${live.length} session${live.length === 1 ? '' : 's'} will be closed` }
      : { bad: false, text: 'No open sessions' },
    refused
      ? { bad: true, text: `Git refused: ${refused}` }
      : wt.info.locked
        ? { bad: true, text: 'Locked with git worktree lock' }
        : { bad: false, text: 'Git refuses if there are uncommitted changes' }
  ]
  const locked = wt.info.locked
  const force = refused !== null || busyAgents > 0 || locked
  function run() {
    close()
    // Why: git refuses a locked worktree unless forced (twice; civetd does that).
    void removeWorktree(hostId, worktree, refused !== null || locked)
  }
  return (
    <>
      <AlertDialogTitle className="text-[14px] leading-normal font-semibold">
        Remove {splitBranch(worktreeBranch(wt.info)).name}?
      </AlertDialogTitle>
      <AlertDialogDescription className="sr-only">{wt.info.path}</AlertDialogDescription>
      {checks.map((c) => (
        <div key={c.text} className="flex gap-[9px] text-[12.5px] text-soft">
          <span className={c.bad ? 'w-3 font-mono text-status-err' : 'w-3 font-mono text-status-ok'}>
            {c.bad ? '!' : '✓'}
          </span>
          <span className="min-w-0">{c.text}</span>
        </div>
      ))}
      <div className="mt-1.5 flex justify-end gap-2">
        <Button type="button" variant="outline" size="md" onClick={close}>
          Cancel
        </Button>
        <Button type="button" size="md" variant={force ? 'destructive' : 'default'} disabled={removing} onClick={run}>
          {force ? 'Remove anyway' : 'Remove'}
        </Button>
      </div>
    </>
  )
}
