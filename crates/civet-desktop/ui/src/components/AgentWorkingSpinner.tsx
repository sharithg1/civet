// Ported from Orca: src/renderer/src/components/AgentWorkingSpinner.tsx
// (phase-sync via Web Animations kept; the .agent-working-spinner keyframes
// live in assets/main.css).
import type React from 'react'
import { cn } from '../lib/utils'

const SPINNER_ANIMATION_NAME = 'agent-spinner-rotate'

function handleSpinnerAnimationStart(event: React.AnimationEvent<HTMLSpanElement>): void {
  if (event.animationName !== SPINNER_ANIMATION_NAME) return
  const el = event.currentTarget
  if (typeof el.getAnimations !== 'function') return
  const animation = el
    .getAnimations()
    .find((a) => 'animationName' in a && (a as CSSAnimation).animationName === SPINNER_ANIMATION_NAME)
  // Why (Orca): anchoring the timeline gives late mounts exact phase sync without recurring JS.
  if (animation) animation.startTime = 0
}

export function AgentWorkingSpinner({ className }: { className?: string }) {
  return (
    <span
      onAnimationStart={handleSpinnerAnimationStart}
      data-agent-spinner=""
      className={cn(
        'agent-working-spinner block rounded-full border-2 border-yellow-500 border-t-transparent motion-reduce:border-t-yellow-500',
        className
      )}
    />
  )
}
