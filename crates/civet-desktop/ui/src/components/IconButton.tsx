// Icon-only button with a shadcn tooltip (replaces the native `title` labels
// of the hand-ported version). The label doubles as the aria-label.
import type * as React from 'react'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

type Props = React.ComponentProps<typeof Button> & {
  label: string
  side?: React.ComponentProps<typeof TooltipContent>['side']
}

export function IconButton({ label, side, className, children, ...props }: Props) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={label}
          className={cn('text-muted-foreground hover:text-foreground', className)}
          {...props}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent side={side} sideOffset={4}>
        {label}
      </TooltipContent>
    </Tooltip>
  )
}
