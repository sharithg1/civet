// In-app toasts for agent attention and errors. Card look ported from Orca's
// src/renderer/src/components/NotificationCardStack.tsx; sonner was dropped.
import { AlertCircle, X } from 'lucide-react'
import { dismissToast, revealPane, useStore } from '../state/store'
import { AgentStateDot } from './AgentStateDot'
import { cn } from '../lib/utils'

export function Toasts() {
  const toasts = useStore((s) => s.toasts)
  return (
    <div className="pointer-events-none fixed right-3 bottom-3 z-40 flex w-80 flex-col gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          role={t.kind === 'error' ? 'alert' : 'status'}
          className={cn(
            'pointer-events-auto flex items-start gap-2 rounded-lg border bg-popover p-3 text-sm text-popover-foreground shadow-floating',
            t.kind === 'error' ? 'border-destructive/40' : 'border-border',
            t.target && 'cursor-pointer hover:bg-accent/40'
          )}
          onClick={() => {
            if (t.target) {
              revealPane(t.target.hostId, t.target.worktree, t.target.pane)
              dismissToast(t.id)
            }
          }}
        >
          <span className="mt-0.5">
            {t.kind === 'error' ? (
              <AlertCircle className="size-3.5 text-destructive" />
            ) : (
              <AgentStateDot state={t.status === 'needsInput' ? 'permission' : 'done'} size="md" />
            )}
          </span>
          <div className="min-w-0 flex-1">
            <div className="truncate font-medium">{t.title}</div>
            {t.body && <div className="line-clamp-3 text-xs text-muted-foreground">{t.body}</div>}
          </div>
          <button
            type="button"
            className="text-muted-foreground hover:text-foreground"
            aria-label="Dismiss"
            onClick={(e) => {
              e.stopPropagation()
              dismissToast(t.id)
            }}
          >
            <X className="size-3.5" />
          </button>
        </div>
      ))}
    </div>
  )
}
