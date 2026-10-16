// Files tab of the right panel: the worktree's tree, read from the daemon a
// directory level at a time (gitignored files hidden unless the eye toggle
// is on) and a quick-open filter over every file. Clicking a file opens it
// read-only in a tab of its own (FileView.tsx); the active one is
// highlighted in the tree.
//
// With a new enough daemon (state/fileops.ts) the tree also manages files:
// new file / folder and rename with an inline name field, duplicate, delete
// (to the daemon's trash, Undo in the toast), cut / copy / paste, drag to
// move (⌥ to copy), Finder drops onto a folder (lib/file-drop.ts) and
// download. ⌘/⇧ click selects several entries; keys: ↑↓ ←→, Enter / F2
// rename, ⌘⌫ delete, ⌘X ⌘C ⌘V, Esc.
//
// Right-click a row (or empty space, for the root) for these and to copy
// its path or open a Terminal tab there. The tree refreshes when the tab is
// shown, when the window regains focus, on the refresh button and right
// after each change made here; there is no file watching.
import { useEffect, useMemo, useRef, useState } from 'react'
import {
  ChevronRight,
  ChevronsDownUp,
  ClipboardPaste,
  Copy,
  CopyPlus,
  Download,
  Eye,
  EyeOff,
  File,
  FileArchive,
  FileCode,
  FileCog,
  FileImage,
  FileJson,
  FileLock,
  FilePlus,
  FileSymlink,
  FileTerminal,
  FileText,
  Folder,
  FolderOpen,
  FolderPlus,
  Pencil,
  RotateCw,
  Scissors,
  Search,
  Trash2,
  X
} from 'lucide-react'
import type { DirListing, FileEntry, FileList } from '@/types'
import { newTerminalTab, openFileTab, pushToast, useStore, wsKey } from '@/state/store'
import { usePrefs } from '@/state/prefs'
import {
  collapseAll,
  expandDir,
  isUnder,
  joinPath,
  loadAllFiles,
  parentDir,
  refreshFiles,
  revealPath,
  setShowIgnored,
  toggleDir,
  useFiles,
  visibleRows,
  type Loadable
} from '@/state/files'
import {
  FILE_OPS_MIN_DAEMON_VERSION,
  copyEntries,
  createEntry,
  downloadEntry,
  duplicateEntry,
  moveEntries,
  nameError,
  pasteEntries,
  renameEntry,
  setFileClip,
  trashEntries,
  useFileClipboard,
  versionAtLeast
} from '@/state/fileops'
import { fuzzyFilter, type FuzzyMatch } from '@/lib/fuzzy'
import { cn } from '@/lib/utils'
import { formatBytes } from './FileView'
import { IconButton } from './IconButton'
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuShortcut,
  ContextMenuTrigger
} from './ui/context-menu'

/** Window focus refreshes at most this often. */
const FOCUS_REFRESH_MS = 2000
const NO_DIRS: string[] = []
/** Pointer travel before a press on a row becomes a drag. */
const DRAG_THRESHOLD = 4

/** Right-clicked entries (none: the root). */
type Target = { paths: string[]; dir: boolean }
/** The inline name field: a new entry in `dir`, or renaming `path`. */
type Editing =
  | { kind: 'create'; dir: string; type: 'file' | 'dir' }
  | { kind: 'rename'; path: string }

const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1)
const ICON_CLS = 'size-3.5 shrink-0 text-quiet'
const rowSelector = (path: string) => `[data-tree-path="${CSS.escape(path)}"]`

export function FilesTab({
  hostId,
  worktree,
  root,
  connected,
  folder = false
}: {
  hostId: string
  worktree: string
  /** The worktree's absolute path on the host. */
  root: string
  connected: boolean
  /** A folder project's root: nothing is gitignored and there are no terminals. */
  folder?: boolean
}) {
  const key = wsKey(hostId, worktree)
  const expanded = usePrefs((s) => s.expandedDirs[key]) ?? NO_DIRS
  const showIgnored = usePrefs((s) => s.showIgnoredFiles)
  const dirs = useFiles((s) => s.dirs[key])
  const active = useStore((s) => {
    const ws = s.workspaces[key]
    const tab = ws?.tabs.find((t) => t.id === ws.activeTab)
    return tab?.kind === 'file' && !tab.diff ? tab.path : null
  })
  // Older daemons: browse only.
  const editable =
    useStore((s) => {
      const v = s.hosts[hostId]?.daemonVersion
      return !!v && versionAtLeast(v, FILE_OPS_MIN_DAEMON_VERSION)
    }) && connected
  const [query, setQuery] = useState('')
  const [sel, setSel] = useState(0)
  const [menu, setMenu] = useState<Target>({ paths: [], dir: true })
  /** Selected rows; `anchor` is the last one clicked (⇧ ranges, keys). */
  const [selection, setSelection] = useState<string[]>([])
  const [anchor, setAnchor] = useState<string | null>(null)
  const [editing, setEditing] = useState<Editing | null>(null)
  const treeRef = useRef<HTMLDivElement>(null)
  /** The menu action opened the name field: closing the menu must not refocus the row. */
  const keepFocus = useRef(false)
  const clip = useFileClipboard((s) => s.clip)
  const cut =
    clip?.mode === 'cut' && clip.hostId === hostId && clip.worktree === worktree
      ? clip.paths
      : NO_DIRS
  const rows = useMemo(() => visibleRows(dirs, expanded), [dirs, expanded])
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

  // Another worktree: start over (state reset during render, not in an effect).
  const [shownKey, setShownKey] = useState(key)
  if (shownKey !== key) {
    setShownKey(key)
    setSelection([])
    setAnchor(null)
    setEditing(null)
  }

  // The selection follows the active file tab.
  useEffect(() => {
    if (!active) return
    setSelection((s) => (s.includes(active) ? s : [active]))
    setAnchor((a) => (a === active ? a : active))
  }, [active])

  const select = (paths: string[], anchorPath: string | null = paths.at(-1) ?? null) => {
    setSelection(paths)
    setAnchor(anchorPath)
  }
  const isDir = (path: string) => rows.find((r) => r.path === path)?.entry.kind === 'dir'
  /** Where new entries and pastes go for a target: the folder itself, else its parent. */
  const dirOf = (path: string | null) => (!path ? '' : isDir(path) ? path : parentDir(path))
  const focusRow = (path: string) =>
    requestAnimationFrame(() =>
      treeRef.current?.querySelector<HTMLElement>(rowSelector(path))?.focus()
    )
  const focusTree = () => requestAnimationFrame(() => treeRef.current?.focus())

  const abs = (path: string) => (path ? `${root.replace(/\/+$/, '')}/${path}` : root)
  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text).catch(() => {})
    pushToast({ kind: 'info', title: `Copied ${text}` }, 3200)
  }
  const open = (path: string, preview = true) => openFileTab(hostId, worktree, path, { preview })
  const pick = (path: string) => {
    revealPath(hostId, worktree, path)
    setQuery('')
    open(path)
  }
  const startCreate = (dir: string, type: 'file' | 'dir') => {
    if (!editable) return
    keepFocus.current = true
    expandDir(hostId, worktree, dir)
    setEditing({ kind: 'create', dir, type })
  }
  const startRename = (path: string) => {
    if (!editable || !path) return
    keepFocus.current = true
    select([path])
    setEditing({ kind: 'rename', path })
  }
  const commitName = async (e: Editing, name: string) => {
    setEditing(null)
    const done =
      e.kind === 'create'
        ? await createEntry(hostId, worktree, e.dir, name, e.type)
        : await renameEntry(hostId, worktree, e.path, name)
    if (done) {
      select([done])
      focusRow(done)
    } else focusTree()
  }
  const remove = (paths: string[]) => {
    if (!editable || !paths.length) return
    select([])
    void trashEntries(hostId, worktree, paths)
  }
  const clipTo = (paths: string[], mode: 'cut' | 'copy') => {
    if (!editable || !paths.length) return
    setFileClip({ hostId, worktree, paths, mode })
  }
  const paste = async (dir: string) => {
    if (!editable) return
    const done = await pasteEntries(hostId, worktree, dir)
    if (done.length) select(done)
  }
  const duplicate = async (path: string) => {
    const done = await duplicateEntry(hostId, worktree, path)
    if (done) select([done])
  }

  const drag = useTreeDrag(editable, async (paths, dest, asCopy) => {
    const done = asCopy
      ? await copyEntries(hostId, worktree, paths, dest)
      : await moveEntries(hostId, worktree, paths, dest)
    if (done.length) select(done)
  })

  const onRowClick = (e: React.MouseEvent, path: string, dir: boolean) => {
    if (e.metaKey || e.ctrlKey) {
      select(
        selection.includes(path) ? selection.filter((p) => p !== path) : [...selection, path],
        path
      )
      return
    }
    if (e.shiftKey && anchor) {
      const order = rows.map((r) => r.path)
      const [a, b] = [order.indexOf(anchor), order.indexOf(path)].sort((x, y) => x - y)
      if (a! >= 0) {
        setSelection(order.slice(a, b! + 1))
        return
      }
    }
    select([path])
    if (dir) toggleDir(hostId, worktree, path)
    else open(path)
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (editing || (e.target as HTMLElement).tagName === 'INPUT') return
    const mod = e.metaKey || e.ctrlKey
    const i = anchor ? rows.findIndex((r) => r.path === anchor) : -1
    const row = rows[i]
    const step = (to: number) => {
      const next = rows[Math.max(0, Math.min(rows.length - 1, to))]
      if (!next) return
      e.preventDefault()
      select([next.path])
      focusRow(next.path)
    }
    if (e.key === 'ArrowDown') return step(i + 1)
    if (e.key === 'ArrowUp') return step(i < 0 ? 0 : i - 1)
    if (e.key === 'ArrowRight' && row?.entry.kind === 'dir') {
      e.preventDefault()
      if (!expanded.includes(row.path)) toggleDir(hostId, worktree, row.path)
      else if (rows[i + 1]?.depth === row.depth + 1) step(i + 1)
      return
    }
    if (e.key === 'ArrowLeft' && row) {
      e.preventDefault()
      if (row.entry.kind === 'dir' && expanded.includes(row.path))
        toggleDir(hostId, worktree, row.path)
      else if (parentDir(row.path)) step(rows.findIndex((r) => r.path === parentDir(row.path)))
      return
    }
    if ((e.key === 'Enter' || e.key === 'F2') && row && editable) {
      // Enter would also click the focused row button.
      e.preventDefault()
      return startRename(row.path)
    }
    if ((e.key === 'Delete' || (mod && e.key === 'Backspace')) && selection.length) {
      e.preventDefault()
      return remove(selection)
    }
    if (mod && (e.key === 'x' || e.key === 'c') && selection.length) {
      e.preventDefault()
      return clipTo(selection, e.key === 'x' ? 'cut' : 'copy')
    }
    if (mod && e.key === 'v') {
      e.preventDefault()
      return void paste(dirOf(anchor))
    }
    if (e.key === 'Escape') {
      if (cut.length) setFileClip(null)
      else select([])
    }
  }

  const rootListing = dirs?.['']

  if (rootListing?.status === 'error' && rootListing.code === 'unsupported') {
    return (
      <div className="px-3 pt-1">
        <Note>{rootListing.message}</Note>
      </div>
    )
  }

  const single = menu.paths.length === 1 ? menu.paths[0]! : null
  const menuDir = single ? (menu.dir ? single : parentDir(single)) : ''
  const many = menu.paths.length > 1

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
        {editable && (
          <>
            <IconButton label="New file" onClick={() => startCreate(dirOf(anchor), 'file')}>
              <FilePlus />
            </IconButton>
            <IconButton label="New folder" onClick={() => startCreate(dirOf(anchor), 'dir')}>
              <FolderPlus />
            </IconButton>
          </>
        )}
        {!folder && (
          <IconButton
            label={showIgnored ? 'Hide ignored files' : 'Show ignored files'}
            onClick={() => setShowIgnored(!showIgnored, connected ? { hostId, worktree } : null)}
            className={showIgnored ? 'text-foreground' : undefined}
          >
            {showIgnored ? <Eye /> : <EyeOff />}
          </IconButton>
        )}
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
        <ContextMenu
          onOpenChange={(open) => {
            if (open) keepFocus.current = false
          }}
        >
          <ContextMenuTrigger asChild>
            <div
              ref={treeRef}
              tabIndex={-1}
              data-tree-root=""
              data-files-host={hostId}
              data-files-worktree={worktree}
              data-files-editable={editable ? '' : undefined}
              data-drop-target={drag.target === '' ? '' : undefined}
              className="relative min-h-0 flex-1 overflow-y-auto px-1.5 pb-3 outline-none"
              onKeyDown={onKeyDown}
              onClick={(e) => {
                if (e.target === e.currentTarget) select([])
              }}
              onContextMenu={(e) => {
                if (e.target === e.currentTarget) {
                  select([])
                  setMenu({ paths: [], dir: true })
                }
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
                  selection={selection}
                  cut={cut}
                  editing={editing}
                  dropTarget={drag.target}
                  onClick={onRowClick}
                  onOpen={open}
                  onMenu={(path, dir) => {
                    const paths = selection.includes(path) ? selection : [path]
                    if (paths !== selection) select(paths)
                    setMenu({ paths, dir: paths.length === 1 && dir })
                  }}
                  onPress={(e, path) =>
                    drag.start(e, () => {
                      if (selection.includes(path)) return selection
                      select([path])
                      return [path]
                    })
                  }
                  onCommit={(e, name) => void commitName(e, name)}
                  onCancel={() => {
                    setEditing(null)
                    focusTree()
                  }}
                />
              )}
            </div>
          </ContextMenuTrigger>
          <ContextMenuContent
            className="w-56"
            onCloseAutoFocus={(e) => {
              if (keepFocus.current) e.preventDefault()
              keepFocus.current = false
            }}
          >
            {single && !menu.dir && (
              <>
                <ContextMenuItem onSelect={() => open(single, false)}>
                  <FileText />
                  Open
                </ContextMenuItem>
                <ContextMenuSeparator />
              </>
            )}
            {editable && !many && (
              <>
                <ContextMenuItem onSelect={() => startCreate(menuDir, 'file')}>
                  <FilePlus />
                  New File…
                </ContextMenuItem>
                <ContextMenuItem onSelect={() => startCreate(menuDir, 'dir')}>
                  <FolderPlus />
                  New Folder…
                </ContextMenuItem>
                <ContextMenuSeparator />
              </>
            )}
            {editable && (
              <>
                {menu.paths.length > 0 && (
                  <>
                    <ContextMenuItem onSelect={() => clipTo(menu.paths, 'cut')}>
                      <Scissors />
                      Cut
                      <ContextMenuShortcut>⌘X</ContextMenuShortcut>
                    </ContextMenuItem>
                    <ContextMenuItem onSelect={() => clipTo(menu.paths, 'copy')}>
                      <Copy />
                      Copy
                      <ContextMenuShortcut>⌘C</ContextMenuShortcut>
                    </ContextMenuItem>
                  </>
                )}
                {!many && (
                  <ContextMenuItem
                    disabled={!clip || clip.hostId !== hostId || clip.worktree !== worktree}
                    onSelect={() => void paste(menuDir)}
                  >
                    <ClipboardPaste />
                    Paste
                    <ContextMenuShortcut>⌘V</ContextMenuShortcut>
                  </ContextMenuItem>
                )}
                {single && (
                  <ContextMenuItem onSelect={() => void duplicate(single)}>
                    <CopyPlus />
                    Duplicate
                  </ContextMenuItem>
                )}
                {menu.paths.length > 0 && (
                  <>
                    <ContextMenuSeparator />
                    {single && (
                      <ContextMenuItem onSelect={() => startRename(single)}>
                        <Pencil />
                        Rename…
                        <ContextMenuShortcut>↩</ContextMenuShortcut>
                      </ContextMenuItem>
                    )}
                    <ContextMenuItem onSelect={() => remove(menu.paths)}>
                      <Trash2 />
                      {many ? `Delete ${menu.paths.length} Items` : 'Delete'}
                      <ContextMenuShortcut>⌘⌫</ContextMenuShortcut>
                    </ContextMenuItem>
                  </>
                )}
                {single && !menu.dir && (
                  <ContextMenuItem onSelect={() => void downloadEntry(hostId, worktree, single)}>
                    <Download />
                    Download…
                  </ContextMenuItem>
                )}
                <ContextMenuSeparator />
              </>
            )}
            {!many && (
              <>
                <ContextMenuItem onSelect={() => copy(abs(single ?? ''))}>
                  <Copy />
                  Copy Path
                </ContextMenuItem>
                <ContextMenuItem disabled={!single} onSelect={() => single && copy(single)}>
                  <Copy />
                  Copy Relative Path
                </ContextMenuItem>
                {!folder && (
                  <>
                    <ContextMenuSeparator />
                    <ContextMenuItem
                      disabled={!connected}
                      onSelect={() => void newTerminalTab(hostId, worktree, abs(menuDir))}
                    >
                      <FileTerminal />
                      Open in Terminal Tab
                    </ContextMenuItem>
                  </>
                )}
              </>
            )}
          </ContextMenuContent>
        </ContextMenu>
      )}
      {drag.ghost && (
        <div
          className="pointer-events-none fixed z-50 rounded-[4px] border border-border bg-muted px-1.5 py-0.5 text-xs text-foreground shadow-floating"
          style={{ left: drag.ghost.x + 12, top: drag.ghost.y + 10 }}
        >
          {drag.ghost.paths.length === 1
            ? baseName(drag.ghost.paths[0]!)
            : `${drag.ghost.paths.length} items`}
          {drag.ghost.copy && ' (copy)'}
        </div>
      )}
    </div>
  )
}

type RowHandlers = {
  onClick: (e: React.MouseEvent, path: string, dir: boolean) => void
  /** Double click: a pinned tab. */
  onOpen: (path: string, preview?: boolean) => void
  onMenu: (path: string, dir: boolean) => void
  onPress: (e: React.PointerEvent, path: string) => void
  onCommit: (e: Editing, name: string) => void
  onCancel: () => void
}

function DirRows({
  dir,
  depth,
  dirs,
  expanded,
  selection,
  cut,
  editing,
  dropTarget,
  ...on
}: {
  dir: string
  depth: number
  dirs: Record<string, Loadable<DirListing>> | undefined
  expanded: string[]
  selection: string[]
  cut: string[]
  editing: Editing | null
  /** Folder an entry is being dragged onto. */
  dropTarget: string | null
} & RowHandlers) {
  const listing = dirs?.[dir]
  const pad = (d: number) => ({ paddingLeft: 6 + d * 12 })
  // Messages line up with the names (past the chevron and icon).
  const textPad = { paddingLeft: 6 + depth * 12 + 34 }
  const creating = editing?.kind === 'create' && editing.dir === dir ? editing : null
  const newRow = creating && (
    <NameInput
      key="new"
      style={pad(depth)}
      initial=""
      icon={
        creating.type === 'dir' ? <Folder className={ICON_CLS} /> : <File className={ICON_CLS} />
      }
      onCommit={(name) => on.onCommit(creating, name)}
      onCancel={on.onCancel}
    />
  )
  if (!listing || listing.status === 'loading')
    return newRow || <Status style={textPad}>Loading…</Status>
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
      {newRow}
      {entries.length === 0 && !newRow && depth === 0 && <Status>No files.</Status>}
      {entries.length === 0 && !newRow && depth > 0 && <Status style={textPad}>Empty</Status>}
      {entries.map((e) => {
        const path = joinPath(dir, e.name)
        const isDir = e.kind === 'dir'
        const isOpen = isDir && expanded.includes(path)
        const renaming = editing?.kind === 'rename' && editing.path === path ? editing : null
        return (
          <div key={e.name}>
            {renaming ? (
              <NameInput
                style={pad(depth)}
                initial={e.name}
                chevron={isDir ? (isOpen ? 'open' : 'closed') : undefined}
                icon={<EntryIcon entry={e} open={isOpen} />}
                onCommit={(name) => on.onCommit(renaming, name)}
                onCancel={on.onCancel}
              />
            ) : (
              <button
                type="button"
                style={pad(depth)}
                title={isDir ? path : `${path} · ${formatBytes(e.size)}`}
                data-tree-path={path}
                data-tree-dir={isDir ? 'true' : 'false'}
                data-drop-target={isDir && dropTarget === path ? '' : undefined}
                aria-selected={selection.includes(path)}
                onClick={(ev) => on.onClick(ev, path, isDir)}
                onDoubleClick={(ev) => {
                  if (!isDir && !ev.metaKey && !ev.ctrlKey && !ev.shiftKey) on.onOpen(path, false)
                }}
                onContextMenu={() => on.onMenu(path, isDir)}
                onPointerDown={(ev) => on.onPress(ev, path)}
                className={cn(
                  'relative flex h-[22px] w-full cursor-pointer items-center gap-1 rounded-[4px] pr-2 text-left text-sm outline-none select-none hover:bg-hover focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-inset',
                  selection.includes(path)
                    ? 'bg-card text-foreground'
                    : e.ignored
                      ? 'text-faint'
                      : 'text-soft',
                  cut.some((c) => isUnder(path, c)) && 'opacity-50'
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
            )}
            {isOpen && (
              <DirRows
                dir={path}
                depth={depth + 1}
                dirs={dirs}
                expanded={expanded}
                selection={selection}
                cut={cut}
                editing={editing}
                dropTarget={dropTarget}
                {...on}
              />
            )}
          </div>
        )
      })}
      {truncated && <Status style={textPad}>Too many entries; the rest aren&apos;t shown.</Status>}
    </>
  )
}

/**
 * The inline name field of a new or renamed entry. Enter or leaving the
 * field commits (an unchanged or empty name cancels), Esc cancels; an
 * invalid name shows why and keeps the field open.
 */
function NameInput({
  initial,
  icon,
  chevron,
  style,
  onCommit,
  onCancel
}: {
  initial: string
  icon: React.ReactNode
  chevron?: 'open' | 'closed'
  style: React.CSSProperties
  onCommit: (name: string) => void
  onCancel: () => void
}) {
  const [value, setValue] = useState(initial)
  const [error, setError] = useState<string | null>(null)
  const done = useRef(false)
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => {
    // After the context menu that opened it has let go of the focus.
    const t = setTimeout(() => {
      const input = ref.current
      if (!input) return
      input.focus()
      // The name without its extension, like Finder.
      const dot = initial.lastIndexOf('.')
      input.setSelectionRange(0, dot > 0 ? dot : initial.length)
    }, 0)
    return () => clearTimeout(t)
  }, [initial])
  const finish = (commit: boolean) => {
    if (done.current) return
    const name = value.trim()
    if (!commit || !name || name === initial) {
      done.current = true
      return onCancel()
    }
    const why = nameError(name)
    if (why) {
      setError(why)
      return
    }
    done.current = true
    onCommit(name)
  }
  return (
    <div style={style} className="relative pr-1">
      <div className="flex h-[22px] items-center gap-1">
        <ChevronRight
          className={cn(
            'size-3 shrink-0 text-dim',
            !chevron && 'invisible',
            chevron === 'open' && 'rotate-90'
          )}
        />
        {icon}
        <input
          ref={ref}
          value={value}
          spellCheck={false}
          aria-label="Name"
          aria-invalid={!!error}
          onChange={(e) => {
            setValue(e.target.value)
            setError(null)
          }}
          onKeyDown={(e) => {
            e.stopPropagation()
            if (e.key === 'Enter') {
              e.preventDefault()
              finish(true)
            } else if (e.key === 'Escape') {
              e.preventDefault()
              finish(false)
            }
          }}
          onBlur={() => {
            // An invalid name can't stay half-edited once the field is left.
            if (nameError(value.trim()) && value.trim()) {
              done.current = true
              onCancel()
            } else finish(true)
          }}
          className={cn(
            'h-[20px] min-w-0 flex-1 rounded-[3px] border bg-background px-1 text-sm text-foreground outline-none',
            error ? 'border-status-err' : 'border-ring'
          )}
        />
      </div>
      {error && <div className="ml-[34px] px-1 pb-0.5 text-xs text-status-err">{error}</div>}
    </div>
  )
}

type Ghost = { paths: string[]; x: number; y: number; copy: boolean }

/**
 * Drag rows onto a folder (or empty space: the root) to move them, ⌥ to
 * copy. Pointer events, not HTML5 drag and drop, which Tauri's file-drop
 * handling can swallow (see lib/reorder.ts). `target` is the folder under
 * the pointer that would take the drop.
 */
function useTreeDrag(
  enabled: boolean,
  onDrop: (paths: string[], dest: string, copy: boolean) => void
) {
  const [state, setState] = useState<{ ghost: Ghost; target: string | null } | null>(null)
  const start = (e: React.PointerEvent, take: () => string[]) => {
    if (!enabled || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return
    const sx = e.clientX
    const sy = e.clientY
    let paths: string[] | null = null
    let target: string | null = null
    const targetAt = (x: number, y: number): string | null => {
      const hit = document.elementFromPoint(x, y)
      const row = hit?.closest<HTMLElement>('[data-tree-path]')
      let dir: string
      if (row) {
        const p = row.dataset.treePath!
        dir = row.dataset.treeDir === 'true' ? p : parentDir(p)
      } else if (hit?.closest('[data-tree-root]')) dir = ''
      else return null
      // Not into itself, nor where everything already is.
      if (paths!.some((p) => dir === p || dir.startsWith(`${p}/`))) return null
      if (paths!.every((p) => parentDir(p) === dir)) return null
      return dir
    }
    const move = (ev: PointerEvent) => {
      if (!paths) {
        if (Math.hypot(ev.clientX - sx, ev.clientY - sy) < DRAG_THRESHOLD) return
        paths = take()
        document.body.style.cursor = 'grabbing'
      }
      target = targetAt(ev.clientX, ev.clientY)
      setState({ ghost: { paths, x: ev.clientX, y: ev.clientY, copy: ev.altKey }, target })
    }
    const end = (ev: PointerEvent) => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', end)
      window.removeEventListener('pointercancel', end)
      document.body.style.cursor = ''
      setState(null)
      if (!paths) return
      // The click that ends a drag is not a click on the row.
      const swallow = (c: MouseEvent) => c.stopPropagation()
      window.addEventListener('click', swallow, { capture: true, once: true })
      setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 0)
      if (ev.type === 'pointerup' && target != null) onDrop(paths, target, ev.altKey)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', end)
    window.addEventListener('pointercancel', end)
  }
  return { start, ghost: state?.ghost ?? null, target: state?.target ?? null }
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
  const cls = ICON_CLS
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
