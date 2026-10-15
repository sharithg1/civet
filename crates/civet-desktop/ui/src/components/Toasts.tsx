// In-app toasts for agent attention and errors, rendered by sonner (shadcn
// `sonner`, see ui/sonner.tsx). The store stays the source of truth
// (pushToast / dismissToast, TTL and the 5-toast cap live there); this bridge
// mirrors store.toasts into sonner. Card look ported from Orca's
// src/renderer/src/components/NotificationCardStack.tsx.
import { useEffect, useRef } from 'react'
import { AlertCircle, X } from 'lucide-react'
import { toast as sonner } from 'sonner'
import { Toaster } from '@/components/ui/sonner'
import { Button } from '@/components/ui/button'
import { dismissToast, revealPane, useStore, type Toast } from '@/state/store'
import { cn } from '@/lib/utils'
import { AgentStateDot } from './AgentStateDot'

export function Toasts() {
  const toasts = useStore((s) => s.toasts)
  const shown = useRef(new Set<number>())

  useEffect(() => {
    const live = new Set(toasts.map((t) => t.id))
    for (const id of shown.current) {
      if (!live.has(id)) {
        shown.current.delete(id)
        sonner.dismiss(id)
      }
    }
    for (const t of toasts) {
      if (shown.current.has(t.id)) continue
      shown.current.add(t.id)
      sonner.custom(() => <ToastCard toast={t} />, {
        id: t.id,
        // The store owns expiry (pushToast's ttlMs).
        duration: Infinity,
        // Swipe-to-dismiss.
        onDismiss: () => dismissToast(t.id)
      })
    }
  }, [toasts])

  return <Toaster />
}

function ToastCard({ toast: t }: { toast: Toast }) {
  return (
    <div
      role={t.kind === 'error' ? 'alert' : 'status'}
      data-toast-kind={t.kind}
      className={cn(
        'flex w-(--width) items-start gap-2 rounded-lg border bg-popover p-3 text-sm text-popover-foreground shadow-floating',
        t.kind === 'error' ? 'border-destructive/40' : 'border-border',
        t.target && 'cursor-pointer hover:bg-[color-mix(in_srgb,var(--accent)_40%,var(--popover))]'
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
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="size-4 text-muted-foreground hover:bg-transparent hover:text-foreground dark:hover:bg-transparent"
        aria-label="Dismiss"
        onClick={(e) => {
          e.stopPropagation()
          dismissToast(t.id)
        }}
      >
        <X className="size-3.5" />
      </Button>
    </div>
  )
}
