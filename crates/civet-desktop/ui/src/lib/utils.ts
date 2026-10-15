// Ported from Orca: src/renderer/src/lib/utils.ts (cn helper, unchanged in spirit).
import { clsx, type ClassValue } from 'clsx'
import { extendTailwindMerge } from 'tailwind-merge'

// Why: tailwind-merge only knows the default font sizes; without this it
// reads `text-2xs` (main.css type scale) as a colour and drops it next to
// `text-faint`.
const twMerge = extendTailwindMerge({ extend: { theme: { text: ['2xs'] } } })

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs))
}

/** Last path segment, for compact labels. */
export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  return trimmed.slice(trimmed.lastIndexOf('/') + 1) || trimmed
}
