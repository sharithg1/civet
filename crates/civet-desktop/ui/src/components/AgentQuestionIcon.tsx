// Ported from Orca: src/renderer/src/components/AgentQuestionIcon.tsx
import type React from 'react'
import { MessageCircleQuestion } from 'lucide-react'
import { cn } from '../lib/utils'

type Props = React.ComponentProps<typeof MessageCircleQuestion>

/** "The agent is asking you something": one icon + one token (--agent-question) everywhere. */
export function AgentQuestionIcon({ className, ...props }: Props) {
  return (
    <MessageCircleQuestion
      {...props}
      className={cn('text-agent-question', className)}
      aria-hidden="true"
    />
  )
}
