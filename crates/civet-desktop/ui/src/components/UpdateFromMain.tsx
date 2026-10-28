// "Update from main" in the Changes tab's branch bar: shown while the
// worktree's branch is behind the default branch (state/lifecycle.ts). It
// opens a menu of Rebase / Merge, the one that suits the branch first
// (rebase while it is unpushed, merge once it is pushed; each item's
// tooltip says why); conflicts are
// reported by a toast (see updateFromMain).
import { ChevronDown, GitBranchPlus, GitMerge } from 'lucide-react'
import type { UpdateStrategy } from '@/types'
import { useStore } from '@/state/store'
import { defaultUpdateStrategy, updateFromMain, useWorktreeLifecycle } from '@/state/lifecycle'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from '@/components/ui/dropdown-menu'

const HINT: Record<UpdateStrategy, string> = {
  rebase: 'Replays your commits on top; a pushed branch then needs a force push',
  merge: 'Adds a merge commit; safe for a pushed branch'
}

export function UpdateFromMainButton({
  hostId,
  worktree,
  disabled
}: {
  hostId: string
  worktree: string
  disabled: boolean
}) {
  const wt = useStore((s) =>
    Object.values(s.worktrees)
      .flat()
      .find((w) => w.id === worktree)
  )
  const defaultBranch = useStore(
    (s) => s.repos[hostId]?.find((r) => r.id === wt?.repo)?.defaultBranch
  )
  const life = useWorktreeLifecycle(hostId, wt?.repo ?? '', worktree)
  if (!wt || !life?.base || !wt.branch || wt.isMain || wt.branch === defaultBranch) return null
  const { behind } = life.info
  if (behind === 0) return null
  const first = defaultUpdateStrategy(life.info)
  const order: UpdateStrategy[] = first === 'rebase' ? ['rebase', 'merge'] : ['merge', 'rebase']
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        disabled={disabled}
        title={`${behind} commit${behind === 1 ? '' : 's'} on ${life.base} not in ${wt.branch}`}
        className="flex h-6 shrink-0 cursor-pointer items-center gap-1 rounded-md px-1.5 text-xs text-quiet outline-none hover:bg-hover-strong hover:text-foreground disabled:pointer-events-none disabled:opacity-50 data-[state=open]:bg-hover-strong [&_svg]:size-3"
      >
        <GitBranchPlus />
        <span className="font-mono">↓{behind}</span>
        <span>{life.base}</span>
        <ChevronDown />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" sideOffset={4} className="w-72">
        <DropdownMenuLabel className="text-xs font-normal text-quiet">
          Update {wt.branch} from {life.base}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        {order.map((st) => (
          <DropdownMenuItem
            key={st}
            title={HINT[st]}
            onSelect={() => void updateFromMain(hostId, worktree, st)}
          >
            {st === 'rebase' ? <GitBranchPlus /> : <GitMerge />}
            {st === 'rebase' ? `Rebase onto ${life.base}` : `Merge ${life.base}`}
            {st === first && <span className="ml-auto text-2xs text-quiet">recommended</span>}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
