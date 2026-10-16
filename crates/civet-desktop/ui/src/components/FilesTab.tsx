// Files tab of the right panel: the worktree's tree, read from the daemon a
// directory level at a time (gitignored files hidden unless the eye toggle
// is on) and a quick-open filter over every file. Clicking a file opens it
// read-only in a tab of its own (FileView.tsx); the active one is
// highlighted in the tree.
//
// Right-click a row (or empty space, for the root) to copy its path or open
// a Terminal tab there. The tree refreshes when the tab is shown, when the
// window regains focus and on the refresh button; there is no file watching.
import { useEffect, useMemo, useState } from 'react'
import {
  ChevronRight,
  ChevronsDownUp,
  Copy,
  Eye,
  EyeOff,
  File,
  FileArchive,
  FileCode,
  FileCog,
  FileImage,
  FileJson,
  FileLock,
  FileSymlink,
  FileTerminal,
  FileText,
  Folder,
  FolderOpen,
  RotateCw,
  Search,
  X
} from 'lucide-react'
import type { DirListing, FileEntry, FileList } from '@/types'
import { newTerminalTab, openFileTab, pushToast, useStore, wsKey } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import {
  collapseAll,
  joinPath,
  loadAllFiles,
  parentDir,
  refreshFiles,
  revealPath,
  setShowIgnored,
  toggleDir,
  useFiles,
  type Loadable
} from '@/state/files'
import { fuzzyFilter, type FuzzyMatch } from '@/lib/fuzzy'
import { cn } from '@/lib/utils'
import { formatBytes } from './FileView'
import { IconButton } from './IconButton'
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger
} from './ui/context-menu'

/** Window focus refreshes at most this often. */
const FOCUS_REFRESH_MS = 2000
const NO_DIRS: string[] = []

type Target = { path: string; dir: boolean }

export function FilesTab({
  hostId,
  worktree,
  root,
  connected
}: {
  hostId: string
  worktree: string
  /** The worktree's absolute path on the host. */
  root: string
  connected: boolean
}) {
  const key = wsKey(hostId, worktree)
  const expanded = usePrefs((s) => s.expandedDirs[key]) ?? NO_DIRS
  const showIgnored = usePrefs((s) => s.showIgnoredFiles)
  const dirs = useFiles((s) => s.dirs[key])
  const selected = useStore((s) => {
    const ws = s.workspaces[key]
    const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
    return tab?.kind === 'file' && !tab.diff ? tab.path : null
  })
  const [query, setQuery] = useState('')
  const [sel, setSel] = useState(0)
  const [menu, setMenu] = useState<Target>({ path: '', dir: true })
  const all = useFiles((s) => s.all[key])
  const paths = all?.status === 'ok' ? all.value.paths : null
  const matches = useMemo(() => (paths && query ? fuzzyFilter(query, paths) : []), [paths, query])

  // Whenever there is a query: also after switching worktrees or reconnecting.
  const searching = query !== ''
  useEffect(() => {
    if (searching && connected) void loadAllFiles(hostId, worktree)
  }, [searching, hostId, worktree, connected])

  useEffect(() => {
    if (!connected) return
    void refreshFiles(hostId, worktree)
    let last = Date.now()
    const onFocus = () => {
      if (Date.now() - last < FOCUS_REFRESH_MS) return
      last = Date.now()
      void refreshFiles(hostId, worktree)
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [hostId, worktree, connected])

  const abs = (path: string) => (path ? `${root.replace(/\/+$/, '')}/${path}` : root)
  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text).catch(() => {})
    pushToast({ kind: 'info', title: `Copied ${text}` }, 3200)
  }
  const open = (path: string) => openFileTab(hostId, worktree, path)
  const pick = (path: string) => {
    revealPath(hostId, worktree, path)
    setQuery('')
    open(path)
  }
  const rootListing = dirs?.['']

  if (rootListing?.status === 'error' && rootListing.code === 'unsupported') {
    return (
      <div className="px-3 pt-1">
        <Note>{rootListing.message}</Note>
      </div>
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-0.5 px-2 pb-1">
        <QuickOpenInput
          query={query}
          onQuery={(q) => {
            setQuery(q)
            setSel(0)
          }}
          onKey={(k) => {
            if (k === 'Enter' && matches[sel]) pick(matches[sel].path)
            else if (k === 'ArrowDown') setSel(Math.min(sel + 1, matches.length - 1))
            else if (k === 'ArrowUp') setSel(Math.max(sel - 1, 0))
          }}
        />
        <IconButton
          label={showIgnored ? 'Hide ignored files' : 'Show ignored files'}
          onClick={() => setShowIgnored(!showIgnored, connected ? { hostId, worktree } : null)}
          className={showIgnored ? 'text-foreground' : undefined}
        >
          {showIgnored ? <Eye /> : <EyeOff />}
        </IconButton>
        <IconButton label="Collapse all" onClick={() => collapseAll(hostId, worktree)}>
          <ChevronsDownUp />
        </IconButton>
        <IconButton
          label="Refresh"
          disabled={!connected}
          onClick={() => void refreshFiles(hostId, worktree)}
        >
          <RotateCw />
        </IconButton>
      </div>

      {query ? (
        <QuickOpen all={all} matches={matches} sel={sel} onSel={setSel} onPick={pick} />
      ) : (
        <ContextMenu>
          <ContextMenuTrigger asChild>
            <div
              className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3"
              onContextMenu={(e) => {
                if (e.target === e.currentTarget) setMenu({ path: '', dir: true })
              }}
            >
              {!rootListing ? (
                <Status>{connected ? 'Loading…' : 'Not connected.'}</Status>
              ) : (
                <DirRows
                  dir=""
                  depth={0}
                  dirs={dirs}
                  expanded={expanded}
                  selected={selected}
                  onToggle={(d) => toggleDir(hostId, worktree, d)}
                  onOpen={open}
                  onMenu={setMenu}
                />
              )}
            </div>
          </ContextMenuTrigger>
          <ContextMenuContent className="w-52">
            {!menu.dir && (
              <>
                <ContextMenuItem onSelect={() => open(menu.path)}>
                  <FileText />
                  Open
                </ContextMenuItem>
                <ContextMenuSeparator />
              </>
            )}
            <ContextMenuItem onSelect={() => copy(abs(menu.path))}>
              <Copy />
              Copy path
            </ContextMenuItem>
            <ContextMenuItem disabled={!menu.path} onSelect={() => copy(menu.path)}>
              <Copy />
              Copy relative path
            </ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem
              disabled={!connected}
              onSelect={() =>
                void newTerminalTab(
                  hostId,
                  worktree,
                  abs(menu.dir ? menu.path : parentDir(menu.path))
                )
              }
            >
              <FileTerminal />
              Open in Terminal tab
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
      )}
    </div>
  )
}

function DirRows({
  dir,
  depth,
  dirs,
  expanded,
  selected,
  onToggle,
  onOpen,
  onMenu
}: {
  dir: string
  depth: number
  dirs: Record<string, Loadable<DirListing>> | undefined
  expanded: string[]
  selected: string | null
  onToggle: (dir: string) => void
  onOpen: (path: string) => void
  onMenu: (t: Target) => void
}) {
  const listing = dirs?.[dir]
  const pad = { paddingLeft: 6 + depth * 12 }
  // Messages line up with the names (past the chevron and icon).
  const textPad = { paddingLeft: 6 + depth * 12 + 34 }
  if (!listing || listing.status === 'loading') return <Status style={textPad}>Loading…</Status>
  if (listing.status === 'error') {
    return (
      <Status style={textPad} error>
        {listing.message}
      </Status>
    )
  }
  const { entries, truncated } = listing.value
  return (
    <>
      {entries.length === 0 && depth === 0 && <Status>No files.</Status>}
      {entries.length === 0 && depth > 0 && <Status style={textPad}>Empty</Status>}
      {entries.map((e) => {
        const path = joinPath(dir, e.name)
        const isDir = e.kind === 'dir'
        const isOpen = isDir && expanded.includes(path)
        return (
          <div key={e.name}>
            <button
              type="button"
              style={pad}
              title={isDir ? path : `${path} · ${formatBytes(e.size)}`}
              onClick={() => (isDir ? onToggle(path) : onOpen(path))}
              onContextMenu={() => onMenu({ path, dir: isDir })}
              className={cn(
                'flex h-[22px] w-full cursor-pointer items-center gap-1 rounded-[4px] pr-2 text-left text-sm hover:bg-hover',
                selected === path
                  ? 'bg-card text-foreground'
                  : e.ignored
                    ? 'text-faint'
                    : 'text-soft'
              )}
            >
              <ChevronRight
                className={cn(
                  'size-3 shrink-0 text-dim transition-transform',
                  !isDir && 'invisible',
                  isOpen && 'rotate-90'
                )}
              />
              <EntryIcon entry={e} open={isOpen} />
              <span className="min-w-0 truncate">{e.name}</span>
            </button>
            {isOpen && (
              <DirRows
                dir={path}
                depth={depth + 1}
                dirs={dirs}
                expanded={expanded}
                selected={selected}
                onToggle={onToggle}
                onOpen={onOpen}
                onMenu={onMenu}
              />
            )}
          </div>
        )
      })}
      {truncated && <Status style={textPad}>Too many entries; the rest aren&apos;t shown.</Status>}
    </>
  )
}

function QuickOpenInput({
  query,
  onQuery,
  onKey
}: {
  query: string
  onQuery: (q: string) => void
  onKey: (key: 'ArrowDown' | 'ArrowUp' | 'Enter') => void
}) {
  return (
    <label className="flex h-[26px] min-w-0 flex-1 items-center gap-1.5 rounded-[5px] border border-border px-2 focus-within:border-ring">
      <Search className="size-3 shrink-0 text-dim" />
      <input
        value={query}
        placeholder="Go to file…"
        spellCheck={false}
        onChange={(e) => onQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') onQuery('')
          // Up / Down / Enter drive the result list.
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Enter') {
            e.preventDefault()
            onKey(e.key)
          }
        }}
        className="min-w-0 flex-1 bg-transparent text-xs text-foreground outline-none placeholder:text-dim"
      />
      {query && (
        <button
          type="button"
          aria-label="Clear"
          onClick={() => onQuery('')}
          className="cursor-pointer text-dim hover:text-foreground"
        >
          <X className="size-3" />
        </button>
      )}
    </label>
  )
}

function QuickOpen({
  all,
  matches,
  sel,
  onSel,
  onPick
}: {
  all: Loadable<FileList> | undefined
  matches: FuzzyMatch[]
  sel: number
  onSel: (i: number) => void
  onPick: (path: string) => void
}) {
  if (!all || all.status === 'loading') return <Status>Loading files…</Status>
  if (all.status === 'error') return <Status error>{all.message}</Status>
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
      {matches.length === 0 && <Status>No matching files.</Status>}
      {matches.map((m, i) => (
        <button
          key={m.path}
          type="button"
          ref={i === sel ? (el) => el?.scrollIntoView({ block: 'nearest' }) : undefined}
          onClick={() => onPick(m.path)}
          onMouseMove={() => i !== sel && onSel(i)}
          className={cn(
            'flex h-[22px] w-full cursor-pointer items-center gap-1.5 rounded-[4px] px-1.5 text-left text-sm',
            i === sel ? 'bg-hover text-foreground' : 'text-soft'
          )}
        >
          <EntryIcon entry={{ name: m.path, kind: 'file' }} open={false} />
          <Highlighted match={m} />
        </button>
      ))}
      {all.value.truncated && (
        <Status>
          Only the first {all.value.paths.length.toLocaleString()} files are searched.
        </Status>
      )}
    </div>
  )
}

/** File name first, then its directory, with the matched characters bold. */
function Highlighted({ match }: { match: FuzzyMatch }) {
  const { path, positions } = match
  const hit = new Set(positions)
  const base = path.lastIndexOf('/') + 1
  const part = (from: number, to: number) => {
    const out: React.ReactNode[] = []
    for (let i = from; i < to; i++) {
      out.push(
        hit.has(i) ? (
          <b key={i} className="font-semibold text-foreground">
            {path[i]}
          </b>
        ) : (
          path[i]
        )
      )
    }
    return out
  }
  return (
    <span className="flex min-w-0 items-baseline gap-1.5">
      <span className="shrink-0">{part(base, path.length)}</span>
      {base > 0 && <span className="min-w-0 truncate text-xs text-quiet">{part(0, base - 1)}</span>}
    </span>
  )
}

const ICONS: [RegExp, typeof File][] = [
  [/\.(lock)$|^(package-lock\.json|pnpm-lock\.yaml)$/i, FileLock],
  [/\.(json|jsonc|json5)$/i, FileJson],
  [/\.(md|mdx|txt|rst|adoc)$|^(readme|license|changelog|notice)$/i, FileText],
  [/\.(png|jpe?g|gif|svg|webp|ico|bmp|tiff?|avif)$/i, FileImage],
  [/\.(zip|tar|gz|tgz|bz2|xz|zst|7z|rar|jar)$/i, FileArchive],
  [/\.(sh|bash|zsh|fish|ps1|bat|cmd)$/i, FileTerminal],
  [
    /\.(toml|ya?ml|ini|cfg|conf|env|editorconfig|properties|plist)$|^(\.gitignore|\.gitattributes|\.env.*|dockerfile|makefile)$/i,
    FileCog
  ],
  [
    /\.(rs|ts|tsx|js|jsx|mjs|cjs|py|go|rb|java|kt|swift|c|h|cc|cpp|hpp|m|mm|cs|php|lua|zig|ex|exs|erl|hs|scala|sql|html|css|scss|vue|svelte)$/i,
    FileCode
  ]
]

function EntryIcon({ entry, open }: { entry: Pick<FileEntry, 'name' | 'kind'>; open: boolean }) {
  const cls = 'size-3.5 shrink-0 text-quiet'
  if (entry.kind === 'dir')
    return open ? <FolderOpen className={cls} /> : <Folder className={cls} />
  if (entry.kind === 'symlink') return <FileSymlink className={cls} />
  return <FileIcon path={entry.name} className={cls} />
}

/** A file-type icon for a path (by name / extension). */
export function FileIcon({ path, className }: { path: string; className?: string }) {
  const name = path.slice(path.lastIndexOf('/') + 1)
  const Icon = ICONS.find(([re]) => re.test(name))?.[1] ?? File
  return <Icon className={className} />
}

function Status({
  children,
  error,
  style
}: {
  children: React.ReactNode
  error?: boolean
  style?: React.CSSProperties
}) {
  return (
    <div
      style={style}
      className={cn('px-3 py-1 text-xs', error ? 'text-status-err' : 'text-faint')}
    >
      {children}
    </div>
  )
}

function Note({ children }: { children: React.ReactNode }) {
  return (
    <div className="rounded-[5px] border border-dashed border-border px-2 py-1.5 text-xs leading-normal text-faint">
      {children}
    </div>
  )
}
