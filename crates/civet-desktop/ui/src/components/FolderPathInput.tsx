// The Add project dialog's Folder field: a text input that lists matching
// subfolders of the selected host (any host, over `list_dirs`) while the
// user types, git repositories marked. Arrow keys move through the list,
// Enter or Tab picks; a picked folder ends in `/` so its own subfolders
// show next. Plain input on hosts that are not connected or run a civetd
// older than LIST_DIRS_MIN_DAEMON_VERSION.
import {
  type ComponentProps,
  type KeyboardEvent as ReactKeyboardEvent,
  useEffect,
  useId,
  useRef,
  useState
} from 'react'
import { Folder, FolderGit2 } from 'lucide-react'
import { Input } from '@/components/ui/input'
import {
  completeFolder,
  folderQuery,
  LIST_DIRS_MIN_DAEMON_VERSION,
  matchFolders
} from '@/lib/folder-complete'
import { cn } from '@/lib/utils'
import { versionAtLeast } from '@/state/fileops'
import { useStore } from '@/state/store'
import type { HostDir } from '@/types'

/** Most suggestions shown at once. */
const MAX_SHOWN = 200

export function FolderPathInput({
  hostId,
  value,
  onValueChange,
  className,
  ...props
}: {
  hostId: string
  value: string
  onValueChange: (path: string) => void
} & Omit<ComponentProps<typeof Input>, 'value' | 'onChange'>) {
  const listId = useId()
  const backend = useStore((s) => s.backend)
  const supported = useStore((s) => {
    const h = s.hosts[hostId]
    return (
      h?.state === 'connected' &&
      !!h.daemonVersion &&
      versionAtLeast(h.daemonVersion, LIST_DIRS_MIN_DAEMON_VERSION)
    )
  })
  // Listings by `${hostId}\0${dir}`: null while loading, 'failed' after an
  // error (a folder that does not exist yet just shows nothing).
  const [listings, setListings] = useState<Record<string, HostDir[] | null | 'failed'>>({})
  const [focused, setFocused] = useState(false)
  const [dismissed, setDismissed] = useState(false)
  const [active, setActive] = useState(-1)
  const listRef = useRef<HTMLDivElement>(null)

  const query = supported ? folderQuery(value) : null
  const dir = query?.dir ?? null
  const key = `${hostId}\u0000${dir}`
  const dirs = listings[key]
  const matches =
    query && Array.isArray(dirs) ? matchFolders(dirs, query.prefix).slice(0, MAX_SHOWN) : []
  const open = focused && !dismissed && matches.length > 0
  const listed = key in listings

  // Each directory is listed once per dialog, shortly after typing stops
  // in it. A failed one is tried again only after moving to another
  // directory and back, never in a loop.
  const currentKey = useRef(key)
  useEffect(() => {
    currentKey.current = key
    setListings((l) => {
      const failed = Object.keys(l).filter((k) => k !== key && l[k] === 'failed')
      if (!failed.length) return l
      const next = { ...l }
      for (const k of failed) delete next[k]
      return next
    })
  }, [key])

  useEffect(() => {
    if (!backend || dir === null || listed) return
    const timer = setTimeout(() => {
      setListings((l) => ({ ...l, [key]: null }))
      backend.listDirs(hostId, dir).then(
        (r) => setListings((l) => ({ ...l, [key]: r.dirs })),
        // Failing after the user moved on: forget it, so coming back retries.
        () =>
          setListings((l) => {
            if (currentKey.current === key) return { ...l, [key]: 'failed' }
            const { [key]: _, ...rest } = l
            return rest
          })
      )
    }, 80)
    return () => clearTimeout(timer)
  }, [backend, hostId, dir, key, listed])

  useEffect(() => {
    if (active >= 0) listRef.current?.children[active]?.scrollIntoView({ block: 'nearest' })
  }, [active])

  // The dialog closes on Escape in a capturing document listener; close
  // the list instead while it is open.
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return
      e.stopPropagation()
      e.preventDefault()
      setDismissed(true)
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [open])

  function change(path: string) {
    onValueChange(path)
    setActive(-1)
    setDismissed(false)
  }

  function pick(i: number) {
    const d = matches[i]
    if (query && d) change(completeFolder(query, d.name))
  }

  function onKeyDown(e: ReactKeyboardEvent<HTMLInputElement>) {
    if (e.nativeEvent.isComposing) return
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (!open) {
        if (dismissed && matches.length) {
          setDismissed(false)
          e.preventDefault()
        }
        return
      }
      e.preventDefault()
      const n = matches.length
      setActive((a) => (e.key === 'ArrowDown' ? (a + 1) % n : a <= 0 ? n - 1 : a - 1))
    } else if (e.key === 'Enter' && open && active >= 0) {
      e.preventDefault()
      pick(active)
    } else if (e.key === 'Tab' && open && !e.shiftKey) {
      e.preventDefault()
      pick(Math.max(active, 0))
    }
  }

  return (
    <div className="relative min-w-0 flex-1">
      <Input
        {...props}
        className={cn('w-full', className)}
        value={value}
        role="combobox"
        aria-autocomplete="list"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-activedescendant={open && active >= 0 ? `${listId}-${active}` : undefined}
        autoComplete="off"
        onChange={(e) => change(e.target.value)}
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
      />
      {open && (
        <div
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label="Folders"
          className="absolute top-full right-0 left-0 z-50 mt-1 max-h-[232px] overflow-y-auto rounded-lg border bg-popover p-1 text-popover-foreground shadow-menu"
        >
          {matches.map((d, i) => (
            <div
              key={d.name}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              className={cn(
                'flex h-[26px] cursor-pointer items-center gap-2 rounded-sm px-2 text-sm select-none hover:bg-hover-strong',
                i === active && 'bg-hover-strong'
              )}
              // Keep focus in the input.
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pick(i)}
            >
              {d.gitRepo ? (
                <FolderGit2 className="size-3.5 shrink-0 text-muted-foreground" />
              ) : (
                <Folder className="size-3.5 shrink-0 text-faint" />
              )}
              <span className="min-w-0 flex-1 truncate font-mono">{d.name}</span>
              {d.gitRepo && (
                <span className="shrink-0 rounded-sm bg-muted px-[5px] text-2xs leading-4 font-medium text-body">
                  git
                </span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
