// "Remove <worktree>?" (v3): lists what would be lost before anything is
// deleted. civetd can't report uncommitted files or unpushed commits yet, so
// the first attempt is a plain `remove_worktree` (git refuses a dirty
// worktree) and only after that refusal does the button become
// "Remove anyway" (force). Live agent sessions are counted client-side.
import { useState } from 'react'
import { panesOfWorktree, removeWorktree, useStore } from '@/state/store'
import { restoreTerminalFocus } from '@/lib/terminal-focus'
import { CivetError } from '@/backend'
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
  const [refused, setRefused] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
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
      : { bad: false, text: 'Git refuses if there are uncommitted changes' }
  ]
  const force = refused !== null || busyAgents > 0
  async function run() {
    setBusy(true)
    try {
      await removeWorktree(hostId, worktree, refused !== null)
      close()
    } catch (err) {
      setRefused(CivetError.from(err).message)
    } finally {
      setBusy(false)
    }
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
        <Button type="button" variant="outline" size="md" onClick={close} disabled={busy}>
          Cancel
        </Button>
        <Button type="button" size="md" variant={force ? 'destructive' : 'default'} disabled={busy} onClick={() => void run()}>
          {force ? 'Remove anyway' : 'Remove'}
        </Button>
      </div>
    </>
  )
}
