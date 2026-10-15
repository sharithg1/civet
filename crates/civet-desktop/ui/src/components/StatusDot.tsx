// Agent status dot of the v3 design: filled for running (blue), waiting for
// you (amber), error (red) and an unread "finished" (green); a hollow ring
// when idle and a dashed ring while the host is unreachable. Replaces the
// Orca-ported AgentStateDot / spinner / question icons.
import React from 'react'
import { cn } from '@/lib/utils'
import { DOT_LABEL, type DotKind } from '@/lib/worktree-view'

const KIND: Record<DotKind, string> = {
  run: 'bg-status-run',
  wait: 'bg-status-wait',
  err: 'bg-status-err',
  ok: 'bg-status-ok',
  idle: 'border-[1.5px] border-dim',
  disc: 'border-[1.5px] border-dashed border-dim'
}

type Props = {
  kind: DotKind
  /** 6px (host), 7px (rows, tabs; default) */
  size?: 6 | 7
  /** Hollow idle ring (sidebar rows); false renders nothing for idle. */
  showIdle?: boolean
  className?: string
  title?: string
}

export const StatusDot = React.memo(function StatusDot({
  kind,
  size = 7,
  showIdle = true,
  className,
  title
}: Props) {
  const hidden = kind === 'idle' && !showIdle
  return (
    <span
      className={cn(
        'inline-block shrink-0 rounded-full',
        size === 6 ? 'size-1.5' : 'size-[7px]',
        hidden ? 'border-0' : KIND[kind],
        className
      )}
      role="img"
      aria-label={title ?? DOT_LABEL[kind]}
      title={title ?? DOT_LABEL[kind]}
      data-status={kind}
    />
  )
})
