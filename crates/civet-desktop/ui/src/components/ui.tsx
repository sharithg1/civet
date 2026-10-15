// Minimal UI primitives with class names ported from Orca's shadcn-style
// components: src/renderer/src/components/ui/button.tsx, input.tsx,
// dialog.tsx and switch.tsx. Radix primitives and cva were dropped in favour
// of plain elements (no portal, focus trap kept minimal).

import { useEffect, useRef, type ReactNode } from 'react'
import { X } from 'lucide-react'
import { cn } from '../lib/utils'

const BUTTON_BASE =
  "inline-flex shrink-0 items-center justify-center gap-2 rounded-md cursor-pointer text-sm font-medium whitespace-nowrap transition-all outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4"

const BUTTON_VARIANTS = {
  default: 'bg-primary text-primary-foreground hover:bg-primary/90',
  destructive: 'bg-destructive text-white hover:bg-destructive/90 dark:bg-destructive/60',
  outline:
    'border border-border bg-background text-foreground shadow-xs hover:bg-accent hover:text-accent-foreground dark:border-input dark:bg-input/30 dark:hover:bg-input/50',
  ghost: 'hover:bg-accent hover:text-accent-foreground dark:hover:bg-accent/50'
} as const

const BUTTON_SIZES = {
  default: 'h-9 px-4 py-2 has-[>svg]:px-3',
  sm: 'h-8 gap-1.5 rounded-md px-3 has-[>svg]:px-2.5',
  xs: 'h-6 gap-1 rounded-md px-2 text-xs'
} as const

export function Button({
  variant = 'default',
  size = 'default',
  className,
  ...props
}: React.ComponentProps<'button'> & {
  variant?: keyof typeof BUTTON_VARIANTS
  size?: keyof typeof BUTTON_SIZES
}) {
  return (
    <button
      type="button"
      {...props}
      className={cn(BUTTON_BASE, BUTTON_VARIANTS[variant], BUTTON_SIZES[size], className)}
    />
  )
}

export function Input({ className, ...props }: React.ComponentProps<'input'>) {
  return (
    <input
      {...props}
      className={cn(
        'h-9 w-full min-w-0 appearance-none rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-xs transition-[color,box-shadow] outline-none placeholder:text-muted-foreground/60 disabled:cursor-not-allowed disabled:opacity-50 dark:bg-input/30',
        'focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50',
        'aria-invalid:border-destructive',
        className
      )}
    />
  )
}

export function Label({ className, ...props }: React.ComponentProps<'label'>) {
  return <label {...props} className={cn('text-xs font-medium text-muted-foreground', className)} />
}

export function Switch({
  checked,
  onChange,
  id,
  disabled
}: {
  checked: boolean
  onChange: (v: boolean) => void
  id?: string
  disabled?: boolean
}) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        'inline-flex h-[1.15rem] w-8 shrink-0 items-center rounded-full border border-transparent shadow-xs transition-all outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:opacity-50',
        checked ? 'bg-primary' : 'bg-input dark:bg-input/80'
      )}
    >
      <span
        className={cn(
          'pointer-events-none block size-4 rounded-full bg-background ring-0 transition-transform dark:bg-foreground',
          checked ? 'translate-x-[calc(100%-2px)] dark:bg-primary-foreground' : 'translate-x-0'
        )}
      />
    </button>
  )
}

export function Dialog({
  title,
  description,
  onClose,
  children,
  className
}: {
  title: string
  description?: ReactNode
  onClose: () => void
  children: ReactNode
  className?: string
}) {
  const ref = useRef<HTMLDivElement>(null)
  const closeRef = useRef(onClose)
  useEffect(() => {
    closeRef.current = onClose
  })
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && closeRef.current()
    window.addEventListener('keydown', onKey)
    ref.current?.querySelector<HTMLElement>('input, select, button[data-autofocus]')?.focus()
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 backdrop-blur-[2px]" onMouseDown={onClose}>
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onMouseDown={(e) => e.stopPropagation()}
        className={cn(
          'relative grid w-full max-w-[calc(100%-2rem)] gap-4 rounded-lg border border-black/14 bg-background/96 p-6 text-foreground shadow-[0_20px_60px_rgba(0,0,0,0.28)] sm:max-w-lg dark:border-white/14 dark:bg-[rgba(23,23,23,0.96)] dark:shadow-[0_24px_72px_rgba(0,0,0,0.55)]',
          className
        )}
      >
        <div className="flex flex-col gap-1.5">
          <h2 className="text-base font-semibold leading-none">{title}</h2>
          {description && <p className="text-sm text-muted-foreground">{description}</p>}
        </div>
        <button
          type="button"
          className="absolute top-4 right-4 rounded-xs opacity-70 transition-opacity hover:opacity-100"
          onClick={onClose}
          aria-label="Close"
        >
          <X className="size-4" />
        </button>
        {children}
      </div>
    </div>
  )
}
