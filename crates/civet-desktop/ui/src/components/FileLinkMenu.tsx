// The menu for a file named in a pane's output (clicked: see
// terminal/file-links): open it in a Civet tab when it is in the pane's
// worktree, open it in its default app on This Mac, or download it from a
// remote host and open it.

import { Copy, Download, ExternalLink, FileText } from 'lucide-react'
import { isLocalHost, openHostFile } from '@/state/fileops'
import { openFileTab, reportError, useStore } from '@/state/store'
import type { FileLink } from '@/terminal/file-links'
import { formatFileSize, relativeTo } from '@/lib/terminal-paths'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger
} from './ui/dropdown-menu'

/** A clicked file link and where the click was (viewport coordinates). */
export type FileLinkTarget = FileLink & { x: number; y: number }

type Props = {
  hostId: string
  /** The pane's worktree: its files also open in a Civet tab. */
  worktree: string | null
  target: FileLinkTarget | null
  onClose: () => void
  /** Once closed: put the focus back in the terminal. */
  onClosed: () => void
}

export function FileLinkMenu({ hostId, worktree, target, onClose, onClosed }: Props) {
  if (!target) return null
  const { file } = target
  const name = file.path.slice(file.path.lastIndexOf('/') + 1)
  const inWorktree = worktree ? relativeTo(worktree, file.path) : null
  const local = isLocalHost(hostId)
  const copyPath = () => {
    useStore
      .getState()
      .backend?.writeClipboard(file.path)
      .catch((err: unknown) => reportError('Could not copy the path', err))
  }
  return (
    <DropdownMenu open onOpenChange={(open) => !open && onClose()}>
      <DropdownMenuTrigger asChild>
        {/* Anchors the menu at the click. */}
        <span
          aria-hidden
          className="pointer-events-none fixed size-0"
          style={{ left: target.x, top: target.y }}
        />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="max-w-[420px] min-w-[220px]"
        onCloseAutoFocus={(e) => {
          e.preventDefault()
          onClosed()
        }}
      >
        <DropdownMenuLabel className="flex items-baseline gap-2 text-xs font-normal text-quiet">
          <span className="min-w-0 truncate" title={file.path}>
            {name}
          </span>
          <span className="shrink-0 text-faint">{formatFileSize(file.size)}</span>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        {worktree && inWorktree && (
          <DropdownMenuItem onSelect={() => openFileTab(hostId, worktree, inWorktree)}>
            <FileText />
            Open in Civet
          </DropdownMenuItem>
        )}
        <DropdownMenuItem onSelect={() => void openHostFile(hostId, file.path)}>
          {local ? <ExternalLink /> : <Download />}
          {local ? 'Open in Default App' : 'Download and Open'}
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={copyPath}>
          <Copy />
          Copy Path
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
