// Cmd+F find bar for one terminal pane: highlights every match in the loaded
// xterm buffer (@xterm/addon-search), steps with Enter / Shift+Enter or
// Cmd+G / Shift+Cmd+G, closes with Esc and hands focus back to the terminal.
// The query and options outlive the bar, so reopening resumes the last search.

import { useCallback, useEffect, useRef, useState } from 'react'
import type { ISearchResultChangeEvent } from '@xterm/addon-search'
import { CaseSensitive, ChevronDown, ChevronUp, Regex, WholeWord, X } from 'lucide-react'
import { Toggle } from '@/components/ui/toggle'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { isMacPlatform } from '@/terminal/clipboard-keys'
import {
  findInputAction,
  findQueryError,
  findResultLabel,
  type FindAction,
  type FindOptions
} from '@/terminal/find-keys'
import { FIND_HIGHLIGHT_LIMIT, type PaneController } from '@/terminal/pane-controller'
import { IconButton } from './IconButton'

export type TerminalFind = {
  open: boolean
  query: string
  options: FindOptions
  results: ISearchResultChangeEvent | null
  error: string | null
  /** Bumped on every Cmd+F so the open bar refocuses and selects its query. */
  focusSeq: number
  setQuery: (query: string) => void
  setOptions: (patch: Partial<FindOptions>) => void
  step: (direction: 'next' | 'previous') => void
  /** A find shortcut from the terminal or the bar. */
  shortcut: (action: FindAction) => void
  close: () => void
}

/** Find state for `controller`'s pane. */
export function useTerminalFind(controller: PaneController | null): TerminalFind {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [options, setOptionsState] = useState<FindOptions>({
    caseSensitive: false,
    regex: false,
    wholeWord: false
  })
  const [results, setResults] = useState<ISearchResultChangeEvent | null>(null)
  const [focusSeq, setFocusSeq] = useState(0)
  const error = findQueryError(query, options)

  // Read by the callbacks below, which the controller holds onto.
  const latest = useRef({ open, query, options, error })
  latest.current = { open, query, options, error }

  useEffect(() => controller?.onFindResults(setResults), [controller])

  // Typing (or toggling an option) re-runs the search, keeping the current match.
  useEffect(() => {
    if (!controller || !open) return
    if (error || !query) {
      controller.clearFind()
      setResults(null)
      return
    }
    controller.find(query, options, 'next', true)
  }, [controller, open, query, options, error])

  const step = useCallback(
    (direction: 'next' | 'previous') => {
      const { query, options, error } = latest.current
      if (!controller || !query || error) return
      controller.find(query, options, direction)
    },
    [controller]
  )

  const shortcut = useCallback(
    (action: FindAction) => {
      if (action === 'open' || !latest.current.open) {
        setOpen(true)
        setFocusSeq((n) => n + 1)
      }
      if (action !== 'open') step(action)
    },
    [step]
  )

  const close = useCallback(() => {
    setOpen(false)
    setResults(null)
    controller?.clearFind()
    controller?.focus()
  }, [controller])

  const setOptions = useCallback(
    (patch: Partial<FindOptions>) => setOptionsState((o) => ({ ...o, ...patch })),
    []
  )

  return {
    open,
    query,
    options,
    results,
    error,
    focusSeq,
    setQuery,
    setOptions,
    step,
    shortcut,
    close
  }
}

export function TerminalFindBar({ find, className }: { find: TerminalFind; className?: string }) {
  const inputRef = useRef<HTMLInputElement>(null)
  const isMac = isMacPlatform()

  useEffect(() => {
    if (find.focusSeq === 0) return
    inputRef.current?.focus()
    inputRef.current?.select()
  }, [find.focusSeq])

  const label = find.error
    ? 'Bad regex'
    : findResultLabel(find.query, find.results, FIND_HIGHLIGHT_LIMIT)
  const canStep = !!find.query && !find.error
  const focusInput = () => {
    inputRef.current?.focus()
    inputRef.current?.select()
  }
  const setOption = (patch: Partial<FindOptions>) => {
    find.setOptions(patch)
    inputRef.current?.focus()
  }

  return (
    <div
      role="search"
      aria-label="Find in terminal"
      className={cn(
        'flex h-7 items-center gap-0.5 rounded-md border border-border bg-popover pr-0.5 pl-2 text-popover-foreground shadow-menu',
        className
      )}
      // Keep clicks from refocusing the pane's xterm (see TerminalPane).
      onMouseDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        const action = findInputAction(e.nativeEvent, isMac)
        // Enter on a button presses it; only the input steps on Enter.
        if (!action || (e.key === 'Enter' && e.target !== inputRef.current)) return
        e.preventDefault()
        // Don't let window listeners (e.g. Settings' own Cmd+F) see it too.
        e.stopPropagation()
        if (action === 'close') find.close()
        else if (action === 'open') focusInput()
        else find.step(action)
      }}
    >
      <input
        ref={inputRef}
        value={find.query}
        onChange={(e) => find.setQuery(e.target.value)}
        placeholder="Find"
        aria-label="Find"
        aria-invalid={!!find.error}
        title={find.error ?? undefined}
        spellCheck={false}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        className="h-6 w-40 min-w-20 shrink bg-transparent font-mono text-xs outline-none placeholder:text-faint aria-invalid:text-status-err-text"
      />
      <span
        className={cn(
          'w-[76px] min-w-0 shrink-[2] truncate pr-1 text-right font-mono text-2xs text-faint tabular-nums',
          (find.error || find.results?.resultCount === 0) && 'text-status-err-text'
        )}
        aria-live="polite"
      >
        {label}
      </span>
      <FindToggle
        label="Match case"
        pressed={find.options.caseSensitive}
        onPressedChange={(caseSensitive) => setOption({ caseSensitive })}
      >
        <CaseSensitive className="size-3.5" />
      </FindToggle>
      <FindToggle
        label="Match whole word"
        pressed={find.options.wholeWord}
        onPressedChange={(wholeWord) => setOption({ wholeWord })}
      >
        <WholeWord className="size-3.5" />
      </FindToggle>
      <FindToggle
        label="Use regular expression"
        pressed={find.options.regex}
        onPressedChange={(regex) => setOption({ regex })}
      >
        <Regex className="size-3.5" />
      </FindToggle>
      <div className="mx-0.5 h-4 w-px bg-border" aria-hidden />
      <FindButton
        label={isMac ? 'Previous match (⇧↩)' : 'Previous match (Shift+Enter)'}
        disabled={!canStep}
        onClick={() => find.step('previous')}
      >
        <ChevronUp className="size-3.5" />
      </FindButton>
      <FindButton
        label={isMac ? 'Next match (↩)' : 'Next match (Enter)'}
        disabled={!canStep}
        onClick={() => find.step('next')}
      >
        <ChevronDown className="size-3.5" />
      </FindButton>
      <FindButton label="Close (Esc)" onClick={find.close}>
        <X className="size-3.5" />
      </FindButton>
    </div>
  )
}

function FindButton({
  label,
  disabled,
  onClick,
  children
}: {
  label: string
  disabled?: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <IconButton
      label={label}
      side="bottom"
      disabled={disabled}
      className="size-6 rounded-[4px] text-quiet hover:bg-hover-strong hover:text-foreground dark:hover:bg-hover-strong"
      onClick={onClick}
    >
      {children}
    </IconButton>
  )
}

function FindToggle({
  label,
  pressed,
  onPressedChange,
  children
}: {
  label: string
  pressed: boolean
  onPressedChange: (pressed: boolean) => void
  children: React.ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Toggle
          aria-label={label}
          pressed={pressed}
          onPressedChange={onPressedChange}
          className="h-6 min-w-6 rounded-[4px] px-0 text-quiet hover:bg-hover-strong hover:text-foreground data-[state=on]:bg-hover-strong data-[state=on]:text-foreground"
        >
          {children}
        </Toggle>
      </TooltipTrigger>
      <TooltipContent side="bottom" sideOffset={4}>
        {label}
      </TooltipContent>
    </Tooltip>
  )
}
