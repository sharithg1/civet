// Adapted from Orca: src/renderer/src/components/ui/sonner.tsx (shadcn sonner
// with popover-token colours). Civet changes: the theme is fixed to dark (the
// app has no theme setting; <html class="dark">), toasts sit 16px from the
// right edge, just above the 24px status bar (v3), stay expanded like the old toast column, and
// Sonner's Alt+T "focus toasts" hotkey is disabled so it never steals focus
// (and the keystroke) from a terminal.
import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon
} from 'lucide-react'
import { Toaster as Sonner, type ToasterProps } from 'sonner'

// Stable reference: Sonner re-subscribes its keydown listener when this changes.
const NO_HOTKEY: string[] = []

const Toaster = ({ ...props }: ToasterProps) => {
  return (
    <Sonner
      theme="dark"
      position="bottom-right"
      offset={{ right: 16, bottom: 34 }}
      mobileOffset={{ right: 16, bottom: 34 }}
      expand
      visibleToasts={5}
      gap={8}
      hotkey={NO_HOTKEY}
      className="toaster group"
      icons={{
        success: <CircleCheckIcon className="size-4" />,
        info: <InfoIcon className="size-4" />,
        warning: <TriangleAlertIcon className="size-4" />,
        error: <OctagonXIcon className="size-4" />,
        loading: <Loader2Icon className="size-4 animate-spin" />
      }}
      style={
        {
          '--normal-bg': 'var(--popover)',
          '--normal-text': 'var(--popover-foreground)',
          '--normal-border': 'var(--border)',
          '--border-radius': 'var(--radius)',
          '--width': '25rem'
        } as React.CSSProperties
      }
      {...props}
    />
  )
}

export { Toaster }
