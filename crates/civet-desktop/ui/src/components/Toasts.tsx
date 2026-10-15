// In-app toasts for agent attention and errors, rendered by sonner (shadcn
// `sonner`, see ui/sonner.tsx). The store stays the source of truth
// (pushToast / dismissToast, TTL and the 5-toast cap live there); this bridge
// mirrors store.toasts into sonner. Card look: Civet v3 toast (#262626 chip
// above the status bar).
import { useEffect, useRef } from 'react'
import { X } from 'lucide-react'
import { toast as sonner } from 'sonner'
import { Toaster } from '@/components/ui/sonner'
import { dismissToast, revealPane, useStore, type Toast } from '@/state/store'
import { cn } from '@/lib/utils'
import { StatusDot } from './StatusDot'

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
        'group/toast ml-auto flex w-max max-w-[400px] items-start gap-2.5 rounded-lg border border-border bg-muted px-[13px] py-[9px] text-[12.5px] text-body shadow-floating',
        t.target && 'cursor-pointer hover:bg-[color-mix(in_srgb,white_4%,var(--muted))]'
      )}
      onClick={() => {
        if (t.target) {
          revealPane(t.target.hostId, t.target.worktree, t.target.pane)
          dismissToast(t.id)
        }
      }}
    >
      {t.kind !== 'info' && (
        <StatusDot
          kind={t.kind === 'error' ? 'err' : t.status === 'needsInput' ? 'wait' : 'ok'}
          className="mt-[5px]"
        />
      )}
      <div className="min-w-0 flex-1">
        <div className={cn(t.kind !== 'info' && 'font-medium text-foreground')}>{t.title}</div>
        {t.body && <div className="mt-0.5 line-clamp-3 text-[12px] text-muted-foreground">{t.body}</div>}
      </div>
      {t.kind !== 'info' && (
        <button
          type="button"
          aria-label="Dismiss"
          className="-mr-1 grid size-4 shrink-0 cursor-pointer place-items-center rounded-[3px] text-dim opacity-0 group-hover/toast:opacity-100 hover:text-foreground focus-visible:opacity-100"
          onClick={(e) => {
            e.stopPropagation()
            dismissToast(t.id)
          }}
        >
          <X className="size-3.5" />
        </button>
      )}
    </div>
  )
}
