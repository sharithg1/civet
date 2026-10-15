// Settings (Civet Settings design, linked from the v3 sidebar / ⌘,). Covers
// the workspace (which stays mounted, so panes stay attached) until "Back to
// workspace" / Esc. Only settings the UI can honour today are shown; startup
// restore, auto-reconnect, worktree location, idle-stop, permission mode and
// updates need backend support (see the follow-up list in the report).
import { useEffect, useRef, useState } from 'react'
import {
  disconnectHost,
  reconnectHost,
  requestRestartDaemon,
  upgradeSummary,
  useStore,
  type SettingsPage
} from '@/state/store'
import { resetPrefs, setPrefs, usePrefs, type Prefs } from '@/state/prefs'
import { focusActiveTerminal } from '@/lib/terminal-focus'
import { cn } from '@/lib/utils'
import { useTitleBarInset } from '@/lib/window-chrome'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { HostStateDot, hostMeta } from './Sidebar'
import { ProjectTile, RingGlyph } from './chrome'

const NAV: [SettingsPage, string][] = [
  ['general', 'General'],
  ['hosts', 'Hosts'],
  ['projects', 'Projects'],
  ['keyboard', 'Keyboard']
]

type Row = {
  id: keyof Prefs
  label: string
  desc?: string
  kind: 'seg' | 'switch' | 'select' | 'input'
  options?: [string, string][]
  ph?: string
}
const SECTIONS: { title: string; rows: Row[] }[] = [
  {
    title: 'Sidebar and switching',
    rows: [
      {
        id: 'worktreeLabel',
        kind: 'seg',
        label: 'Worktree names',
        desc: "Task title is the agent's terminal title. The other one is shown as secondary text on the selected worktree.",
        options: [
          ['task', 'Task title'],
          ['branch', 'Branch']
        ]
      },
      {
        id: 'sidebarTabs',
        kind: 'seg',
        label: 'Show tabs under',
        options: [
          ['selected', 'Selected worktree'],
          ['all', 'Every worktree'],
          ['none', 'Nowhere']
        ]
      },
      {
        id: 'recentCount',
        kind: 'select',
        label: 'Recent worktrees in switcher',
        options: [
          ['3', '3'],
          ['5', '5'],
          ['8', '8'],
          ['10', '10']
        ]
      }
    ]
  },
  {
    title: 'Sessions',
    rows: [
      {
        id: 'agentCommand',
        kind: 'input',
        label: 'Claude command',
        desc: "Used for new Claude tabs (⌥C) and as the on-create step for projects that don't save their own.",
        ph: 'claude'
      },
      { id: 'confirmCloseRunning', kind: 'switch', label: 'Confirm before closing a running agent' }
    ]
  },
  {
    title: 'Terminal',
    rows: [
      {
        id: 'prediction',
        kind: 'seg',
        label: 'Local echo prediction',
        desc: 'Auto shows what you type right away (underlined until the host confirms it) on connections slower than ~30 ms, like mosh. Off always waits for the host to echo.',
        options: [
          ['auto', 'Auto'],
          ['off', 'Off']
        ]
      }
    ]
  }
]

const KEYS: [string, string][] = [
  ['⌘K', 'Switch worktree'],
  ['⌥`', 'Previous worktree'],
  ['⌥↑ / ⌥↓', 'Previous / next worktree'],
  ['⌥1 … ⌥9', 'Go to tab'],
  ['⌥C', 'New Claude tab'],
  ['⌥T / ⌘T', 'New terminal tab'],
  ['⌘D / ⇧⌘D', 'Split right / down'],
  ['⌘W', 'Close focused pane'],
  ['⌥N', 'New worktree'],
  ['⌥B', 'Toggle sidebar'],
  ['⌥E', 'Toggle right panel'],
  ['⌘,', 'Settings'],
  ['⌘C / ⌘A', 'Copy selection / select all in a terminal']
]

export function SettingsView() {
  const page = useStore((s) => s.settings)
  const [q, setQ] = useState('')
  const [saved, setSaved] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const prefs = usePrefs()
  const searchRef = useRef<HTMLInputElement>(null)
  const inset = useTitleBarInset()
  const close = () => {
    useStore.setState({ settings: null })
    setTimeout(focusActiveTerminal, 0)
  }
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.code === 'KeyF') {
        e.preventDefault()
        searchRef.current?.focus()
      } else if (e.key === 'Escape' && !e.defaultPrevented && !document.querySelector('[role=listbox]')) {
        const t = e.target as HTMLElement
        if (t.tagName === 'INPUT' && (t as HTMLInputElement).value) return
        useStore.setState({ settings: null })
        setTimeout(focusActiveTerminal, 0)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  if (!page) return null
  const set = (patch: Partial<Prefs>) => {
    setPrefs(patch)
    setSaved(true)
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(() => setSaved(false), 1600)
  }
  const query = q.trim().toLowerCase()
  const sections = SECTIONS.map((sec) => ({
    ...sec,
    rows: sec.rows.filter((r) => !query || `${r.label} ${r.desc ?? ''} ${sec.title}`.toLowerCase().includes(query))
  })).filter((s) => s.rows.length)
  const showGeneral = page === 'general' || !!query
  return (
    <div className="absolute inset-0 z-40 flex flex-col bg-chrome" role="region" aria-label="Settings">
      <div
        data-tauri-drag-region="deep"
        className="flex h-10 shrink-0 items-center gap-3 border-b border-border px-3.5 select-none"
        style={inset ? { paddingLeft: inset } : undefined}
      >
        <span className="text-sm font-semibold text-foreground">Civet</span>
        <span className="text-dim">/</span>
        <span className="text-sm text-muted-foreground">Settings</span>
        <span className="flex-1" />
        <span className="text-xs text-faint">{saved ? 'Saved' : 'Changes save automatically'}</span>
      </div>
      <div className="flex min-h-0 flex-1">
        <nav className="flex w-[220px] shrink-0 flex-col gap-0.5 border-r border-border p-2.5">
          <button
            type="button"
            onClick={close}
            className="mb-1 flex h-7 cursor-pointer items-center gap-1.5 rounded-md px-2 text-sm text-muted-foreground hover:bg-hover hover:text-foreground"
          >
            ← Back to workspace<span className="flex-1" />
            <span className="font-mono text-2xs text-dim">esc</span>
          </button>
          <div className="mb-2 flex h-8 items-center gap-2 rounded-lg border border-border bg-white/[0.045] px-2.5">
            <RingGlyph />
            <input
              ref={searchRef}
              value={q}
              onChange={(e) => {
                setQ(e.target.value)
                if (e.target.value) useStore.setState({ settings: 'general' })
              }}
              placeholder="Search settings"
              aria-label="Search settings"
              className="min-w-0 flex-1 border-none bg-transparent text-sm text-foreground outline-none placeholder:text-faint"
            />
          </div>
          {NAV.map(([id, label]) => (
            <button
              key={id}
              type="button"
              onClick={() => {
                setQ('')
                useStore.setState({ settings: id })
              }}
              className={cn(
                'flex h-7 cursor-pointer items-center rounded-md px-2.5 text-sm',
                page === id ? 'bg-accent text-foreground' : 'text-muted-foreground hover:bg-hover'
              )}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="min-h-0 flex-1 overflow-y-auto bg-background px-10 py-8">
          <div className="max-w-[640px]">
            <h1 className="text-xl font-semibold text-foreground">
              {showGeneral ? 'General' : NAV.find((n) => n[0] === page)![1]}
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {showGeneral
                ? 'Switching, sidebar and defaults for new sessions.'
                : page === 'hosts'
                  ? 'Machines running civetd. Closing Civet detaches; sessions keep running.'
                  : page === 'projects'
                    ? 'Repositories on each host, and their saved on-create steps.'
                    : 'Shortcuts work everywhere, including inside terminals.'}
            </p>
            {showGeneral &&
              sections.map((sec) => (
                <section key={sec.title} className="mt-7">
                  <h2 className="mb-2.5 text-sm font-semibold text-foreground">{sec.title}</h2>
                  <Card>
                    {sec.rows.map((r) => (
                      <SettingRow key={r.id} row={r} value={prefs[r.id]} onChange={(v) => set({ [r.id]: v } as Partial<Prefs>)} />
                    ))}
                  </Card>
                </section>
              ))}
            {showGeneral && !sections.length && <p className="mt-7 text-sm text-faint">No settings match.</p>}
            {showGeneral && !query && (
              <button
                type="button"
                onClick={() => {
                  resetPrefs()
                  setSaved(true)
                }}
                className="mt-5 h-[26px] cursor-pointer rounded-[5px] border border-border px-2.5 text-xs text-soft hover:bg-hover"
              >
                Reset to defaults
              </button>
            )}
            {!showGeneral && page === 'hosts' && <HostsPage />}
            {!showGeneral && page === 'projects' && <ProjectsPage />}
            {!showGeneral && page === 'keyboard' && (
              <Card className="mt-7">
                {KEYS.map(([k, label]) => (
                  <div key={k} className="flex items-center px-4 py-2.5 text-sm text-body">
                    {label}
                    <span className="flex-1" />
                    <span className="font-mono text-xs text-muted-foreground">{k}</span>
                  </div>
                ))}
              </Card>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

function Card({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div className={cn('divide-y divide-white/[0.08] rounded-xl border border-border bg-chrome', className)}>
      {children}
    </div>
  )
}

function SettingRow({ row, value, onChange }: { row: Row; value: unknown; onChange: (v: unknown) => void }) {
  return (
    <div className="flex items-center gap-6 px-4 py-3.5">
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium text-foreground">{row.label}</div>
        {row.desc && <div className="mt-1 text-xs leading-normal text-muted-foreground">{row.desc}</div>}
      </div>
      {row.kind === 'seg' && (
        <div className="flex shrink-0 gap-0.5 rounded-md bg-muted p-0.5" role="radiogroup" aria-label={row.label}>
          {row.options!.map(([k, l]) => (
            <button
              key={k}
              type="button"
              role="radio"
              aria-checked={value === k}
              onClick={() => onChange(k)}
              className={cn(
                'h-6 cursor-pointer rounded-sm px-2.5 text-sm',
                value === k ? 'bg-field text-foreground shadow-[0_1px_2px_rgba(0,0,0,0.4)]' : 'text-muted-foreground'
              )}
            >
              {l}
            </button>
          ))}
        </div>
      )}
      {row.kind === 'switch' && (
        <Switch checked={value === true} onCheckedChange={(v) => onChange(v)} aria-label={row.label} />
      )}
      {row.kind === 'select' && (
        <Select value={String(value)} onValueChange={(v) => onChange(Number(v))}>
          <SelectTrigger aria-label={row.label} className="h-8 w-[170px] text-sm">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {row.options!.map(([k, l]) => (
              <SelectItem key={k} value={k}>
                {l}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
      {row.kind === 'input' && (
        <Input
          aria-label={row.label}
          value={String(value)}
          placeholder={row.ph}
          spellCheck={false}
          onChange={(e) => onChange(e.target.value)}
          className="h-8 w-[240px] rounded-[5px] border-border bg-field font-mono text-sm shadow-none dark:bg-field"
        />
      )}
    </div>
  )
}

function HostsPage() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const btn = 'h-[26px] cursor-pointer rounded-[5px] border border-border px-2.5 text-xs text-soft hover:bg-hover'
  return (
    <>
      <Card className="mt-7">
        {hostOrder.map((id) => {
          const h = hosts[id]
          if (!h) return null
          return (
            <div key={id} className="flex items-center gap-3 px-4 py-3">
              <HostStateDot host={h} />
              <div className="min-w-0 flex-1">
                <div className="text-sm font-medium text-foreground">{h.label}</div>
                <div className="font-mono text-xs text-faint">
                  {h.hostId} · {h.state} · {hostMeta(h)}
                  {h.transport ? ` · ${h.transport === 'quic' ? 'QUIC' : 'SSH tunnel'}` : ''}
                  {h.daemonVersion ? ` · civetd ${h.daemonVersion}` : ''}
                </div>
                {h.upgrade && (
                  <div className="mt-0.5 font-mono text-xs text-status-wait">{upgradeSummary(id)}</div>
                )}
              </div>
              {(h.state === 'failed' || h.state === 'disconnected') && (
                <button type="button" className={btn} onClick={() => void reconnectHost(id)}>
                  Reconnect
                </button>
              )}
              {(h.state === 'connected' || h.upgrade || h.errorCode === 'daemonBusy') && (
                <button type="button" className={btn} onClick={() => requestRestartDaemon(id)}>
                  Restart daemon…
                </button>
              )}
              <button type="button" className={btn} onClick={() => useStore.setState({ addProject: { hostId: id } })}>
                Add project…
              </button>
              <button type="button" className={btn} onClick={() => void disconnectHost(id)}>
                Disconnect
              </button>
            </div>
          )
        })}
        {hostOrder.length === 0 && <div className="px-4 py-3 text-sm text-faint">No hosts.</div>}
      </Card>
      <button type="button" className={cn(btn, 'mt-4')} onClick={() => useStore.setState({ showConnect: true })}>
        Connect a host…
      </button>
    </>
  )
}

function ProjectsPage() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const repos = useStore((s) => s.repos)
  const steps = usePrefs((s) => s.createSteps)
  return (
    <Card className="mt-7">
      {hostOrder.flatMap((h) =>
        (repos[h] ?? []).map((r) => {
          const saved = steps[`${h}\u0000${r.id}`]
          return (
            <div key={`${h}:${r.id}`} className="flex items-center gap-3 px-4 py-3">
              <ProjectTile name={r.name} />
              <div className="min-w-0 flex-1">
                <div className="text-sm font-medium text-foreground">
                  {r.name} <span className="font-normal text-faint">· {hosts[h]?.label ?? h}</span>
                </div>
                <div className="truncate font-mono text-xs text-faint">
                  {r.path} · default {r.defaultBranch ?? 'HEAD'}
                </div>
              </div>
              <div className="max-w-[220px] truncate font-mono text-xs text-muted-foreground" title="Saved on-create steps">
                {saved ? saved.map((s) => s.cmd || s.kind).join(' → ') || 'no steps' : 'default steps'}
              </div>
            </div>
          )
        })
      )}
    </Card>
  )
}
