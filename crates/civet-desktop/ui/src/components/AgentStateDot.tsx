// Ported from Orca: src/renderer/src/components/AgentStateDot.tsx and
// src/renderer/src/components/sidebar/StatusIndicator.tsx, merged and reduced
// to the states Civet's daemon reports (running → working, needsInput →
// permission, done, idle → active, unknown → inactive). Orca's Radix tooltip
// (StateIndicatorTooltip) is replaced by the native title attribute.
import React from 'react'
import { CircleCheck } from 'lucide-react'
import { cn } from '../lib/utils'
import type { IndicatorStatus } from '../lib/agent-status'
import { AgentQuestionIcon } from './AgentQuestionIcon'
import { AgentWorkingSpinner } from './AgentWorkingSpinner'

export function indicatorLabel(state: IndicatorStatus): string {
  switch (state) {
    case 'working':
      return 'Working'
    case 'permission':
      return 'Needs input'
    case 'done':
      return 'Done'
    case 'active':
      return 'Idle'
    case 'inactive':
      return 'No agent activity'
  }
}

type Props = {
  state: IndicatorStatus
  size?: 'sm' | 'md'
  className?: string
  title?: string | null
}

export const AgentStateDot = React.memo(function AgentStateDot({
  state,
  size = 'sm',
  className,
  title
}: Props) {
  const box = size === 'md' ? 'h-3 w-3' : 'h-2.5 w-2.5'
  const inner = size === 'md' ? 'size-2' : 'size-1.5'
  const icon = size === 'md' ? 'size-3' : 'size-2.5'
  const label = indicatorLabel(state)
  let glyph: React.JSX.Element
  if (state === 'working') {
    glyph = <AgentWorkingSpinner className={inner} />
  } else if (state === 'done') {
    // Why (Orca): a check keeps completion visually distinct from idle.
    glyph = <CircleCheck className={cn('text-emerald-500', icon)} aria-hidden="true" />
  } else if (state === 'permission') {
    glyph = <AgentQuestionIcon className={icon} />
  } else {
    glyph = (
      <span
        className={cn(
          'block rounded-full',
          inner,
          state === 'active' ? 'bg-emerald-500/70' : 'bg-neutral-500/40'
        )}
      />
    )
  }
  return (
    <span
      className={cn('inline-flex shrink-0 items-center justify-center', box, className)}
      aria-label={label}
      title={title === null ? undefined : (title ?? label)}
      data-agent-state={state}
    >
      {glyph}
    </span>
  )
})
