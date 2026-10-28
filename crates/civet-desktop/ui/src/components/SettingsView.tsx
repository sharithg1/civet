// Settings (linked from the v3 sidebar / ⌘,), laid out like Cursor's: a
// full-height nav column (Back, search, pages with icons) beside a centred
// column of titled cards whose rows carry a label, a description and a
// right-aligned control (dropdown, switch or field). Covers the workspace
// (which stays mounted, so panes stay attached) until Back / Esc. Only settings the UI can honour today are shown; startup
// restore, auto-reconnect, worktree location, idle-stop, permission mode and
// updates need backend support (see the follow-up list in the report).
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { ArrowLeft, Folder, Keyboard, Search, Server, Settings } from 'lucide-react'
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
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue
} from '@/components/ui/select'
import { HostStateDot, hostMeta } from './Sidebar'
import { ProjectTile } from './chrome'
import { WorktreeTemplateEditor } from './WorktreeTemplateEditor'
import { hasWorktreeTemplates } from '@/state/templates'

const NAV: [SettingsPage, string, ReactNode][] = [
  ['general', 'General', <Settings key="general" />],
  ['hosts', 'Hosts', <Server key="hosts" />],
  ['projects', 'Projects', <Folder key="projects" />],
  ['keyboard', 'Keyboard', <Keyboard key="keyboard" />]
]

type Row = {
  id: keyof Prefs
  label: string
  desc?: string
  kind: 'choice' | 'switch' | 'select' | 'input'
  options?: [string, string][]
  ph?: string
}
const SECTIONS: { title: string; rows: Row[] }[] = [
  {
    title: 'Sidebar and switching',
    rows: [
      {
        id: 'worktreeLabel',
        kind: 'choice',
        label: 'Worktree names',
        desc: "Task title is the agent's terminal title. The other one is shown as secondary text on the selected worktree.",
        options: [
          ['task', 'Task title'],
          ['branch', 'Branch']
        ]
      },
      {
        id: 'sidebarTabs',
        kind: 'choice',
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
      {
        id: 'codexCommand',
        kind: 'input',
        label: 'Codex command',
        desc: 'Used for new Codex tabs (⌥X) and Codex on-create steps.',
        ph: 'codex'
      },
      { id: 'confirmCloseRunning', kind: 'switch', label: 'Confirm before closing a running agent' }
    ]
  },
  {
    title: 'Host stats',
    rows: [
      {
        id: 'showHostStats',
        kind: 'switch',
        label: 'Show host stats in the title bar',
        desc: "CPU and memory of the selected worktree's host; click it for the full stats view. Off also stops asking the host for them."
      },
      {
        id: 'hostStatsSparkline',
        kind: 'switch',
        label: 'CPU graph in the title bar',
        desc: 'The last minute of CPU use next to the numbers.'
      }
    ]
  },
  {
    title: 'Terminal',
    rows: [
      {
        id: 'prediction',
        kind: 'choice',
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
  ['⇧⌘B', 'Send prompt to agents (⌘-click worktrees to pick several)'],
  ['⌥`', 'Previous worktree'],
  ['⌥↑ / ⌥↓', 'Previous / next worktree'],
  ['⌥1 … ⌥9', 'Go to tab'],
  ['⌥C', 'New Claude tab'],
  ['⌥X', 'New Codex tab'],
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
  const close = () => {
    useStore.setState({ settings: null })
    setTimeout(focusActiveTerminal, 0)
  }
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.code === 'KeyF') {
        e.preventDefault()
        searchRef.current?.focus()
      } else if (
        e.key === 'Escape' &&
        !e.defaultPrevented &&
        !document.querySelector('[role=listbox]')
      ) {
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
    rows: sec.rows.filter(
      (r) => !query || `${r.label} ${r.desc ?? ''} ${sec.title}`.toLowerCase().includes(query)
    )
  })).filter((s) => s.rows.length)
  const showGeneral = page === 'general' || !!query
  const title = showGeneral ? 'General' : NAV.find((n) => n[0] === page)![1]
  return (
    <div className="absolute inset-0 z-40 flex bg-background" role="region" aria-label="Settings">
      <nav className="flex w-[260px] shrink-0 flex-col border-r border-border bg-sidebar text-sidebar-foreground">
        <div data-tauri-drag-region="deep" className="h-10 shrink-0 select-none" />
        <div className="flex min-h-0 flex-1 flex-col gap-px px-2">
          <button
            type="button"
            onClick={close}
            title="Back (esc)"
            className={cn(navItem, 'text-quiet')}
          >
            <ArrowLeft />
            Back
          </button>
          <div className="my-2 flex h-[30px] items-center gap-2 rounded-md border border-border bg-field px-2 focus-within:border-border-strong">
            <Search className="size-4 shrink-0 text-faint" />
            <input
              ref={searchRef}
              value={q}
              onChange={(e) => {
                setQ(e.target.value)
                if (e.target.value) useStore.setState({ settings: 'general' })
              }}
              placeholder="Search Settings"
              aria-label="Search settings"
              className="min-w-0 flex-1 border-none bg-transparent text-base text-foreground outline-none placeholder:text-faint"
            />
          </div>
          {NAV.map(([id, label, icon]) => (
            <button
              key={id}
              type="button"
              aria-current={page === id && !query ? 'page' : undefined}
              onClick={() => {
                setQ('')
                useStore.setState({ settings: id })
              }}
              className={cn(
                navItem,
                page === id && !query && 'bg-sidebar-accent text-foreground hover:bg-sidebar-accent'
              )}
            >
              {icon}
              {label}
            </button>
          ))}
        </div>
        <div className="flex h-12 shrink-0 items-center px-4 text-xs text-faint">
          {saved ? 'Saved' : 'Changes save automatically'}
        </div>
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        <div data-tauri-drag-region="deep" className="h-10 shrink-0 select-none" />
        <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-16">
          <div className="mx-auto max-w-[780px] pt-4">
            <h1 className="px-2 text-lg font-medium text-foreground">{title}</h1>
            {showGeneral &&
              sections.map((sec) => (
                <Section key={sec.title} title={sec.title}>
                  <Card>
                    {sec.rows.map((r) => (
                      <SettingRow
                        key={r.id}
                        row={r}
                        value={prefs[r.id]}
                        onChange={(v) => set({ [r.id]: v } as Partial<Prefs>)}
                      />
                    ))}
                  </Card>
                </Section>
              ))}
            {showGeneral && !sections.length && (
              <p className="mt-6 px-2 text-sm text-faint">No settings match.</p>
            )}
            {showGeneral && !query && (
              <button
                type="button"
                onClick={() => {
                  resetPrefs()
                  setSaved(true)
                }}
                className={cn(btn, 'mt-6')}
              >
                Reset to defaults
              </button>
            )}
            {!showGeneral && page === 'hosts' && <HostsPage />}
            {!showGeneral && page === 'projects' && <ProjectsPage />}
            {!showGeneral && page === 'keyboard' && (
              <Section title="Shortcuts work everywhere, including inside terminals">
                <Card>
                  {KEYS.map(([k, label]) => (
                    <div key={k} className="flex items-center px-1 py-2.5 text-sm text-body">
                      {label}
                      <span className="flex-1" />
                      <span className="font-mono text-xs text-muted-foreground">{k}</span>
                    </div>
                  ))}
                </Card>
              </Section>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

const navItem =
  'flex h-[30px] cursor-pointer items-center gap-2.5 rounded-md px-2 text-left text-base text-soft hover:bg-hover hover:text-foreground [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-quiet'

const btn =
  'h-7 cursor-pointer rounded-md border border-border px-2.5 text-sm text-soft hover:bg-hover hover:text-foreground'

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="mt-6">
      <h2 className="mb-2 px-2 text-sm text-muted-foreground">{title}</h2>
      {children}
    </section>
  )
}

/** Rows inset from the card edge, split by hairlines that stop at the inset (as in Cursor). */
function Card({ children }: { children: ReactNode }) {
  return (
    <div className="divide-y divide-foreground/[0.07] rounded-lg bg-chrome px-3 dark:bg-card">
      {children}
    </div>
  )
}

function SettingRow({
  row,
  value,
  onChange
}: {
  row: Row
  value: unknown
  onChange: (v: unknown) => void
}) {
  return (
    <div className="flex min-h-[60px] items-center gap-6 px-1 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="text-sm text-foreground">{row.label}</div>
        {row.desc && <div className="mt-0.5 text-sm text-muted-foreground">{row.desc}</div>}
      </div>
      {(row.kind === 'select' || row.kind === 'choice') && (
        <Select
          value={String(value)}
          onValueChange={(v) => onChange(row.kind === 'select' ? Number(v) : v)}
        >
          <SelectTrigger
            aria-label={row.label}
            className="h-7 gap-1 rounded-md border-border bg-transparent px-2 text-sm text-foreground shadow-none dark:bg-transparent dark:hover:bg-hover [&_svg]:size-3.5"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent position="popper" align="end">
            {row.options!.map(([k, l]) => (
              <SelectItem key={k} value={k}>
                {l}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
      {row.kind === 'switch' && (
        <Switch
          checked={value === true}
          onCheckedChange={(v) => onChange(v)}
          aria-label={row.label}
          className="h-5 w-9 data-[state=checked]:bg-status-ok dark:data-[state=unchecked]:bg-input [&>span]:size-4 [&>span]:bg-white [&>span]:data-[state=checked]:translate-x-[17px] [&>span]:data-[state=unchecked]:translate-x-px dark:[&>span]:data-[state=checked]:bg-white dark:[&>span]:data-[state=unchecked]:bg-white"
        />
      )}
      {row.kind === 'input' && (
        <Input
          aria-label={row.label}
          value={String(value)}
          placeholder={row.ph}
          spellCheck={false}
          onChange={(e) => onChange(e.target.value)}
          className="h-7 w-[220px] rounded-md border-border bg-transparent px-2 font-mono text-sm shadow-none dark:bg-transparent"
        />
      )}
    </div>
  )
}

function HostsPage() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  return (
    <Section title="Machines running civetd. Closing Civet detaches; sessions keep running">
      <Card>
        {hostOrder.map((id) => {
          const h = hosts[id]
          if (!h) return null
          return (
            <div key={id} className="flex items-center gap-3 px-1 py-3">
              <HostStateDot host={h} />
              <div className="min-w-0 flex-1">
                <div className="text-sm font-medium text-foreground">{h.label}</div>
                <div className="font-mono text-xs text-faint">
                  {h.hostId} · {h.state} · {hostMeta(h)}
                  {h.transport ? ` · ${h.transport === 'quic' ? 'QUIC' : 'SSH tunnel'}` : ''}
                  {h.daemonVersion ? ` · civetd ${h.daemonVersion}` : ''}
                </div>
                {h.upgrade && (
                  <div className="mt-0.5 font-mono text-xs text-status-wait">
                    {upgradeSummary(id)}
                  </div>
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
              <button
                type="button"
                className={btn}
                onClick={() => useStore.setState({ addProject: { hostId: id } })}
              >
                Add project…
              </button>
              <button type="button" className={btn} onClick={() => void disconnectHost(id)}>
                Disconnect
              </button>
            </div>
          )
        })}
        {hostOrder.length === 0 && <div className="px-1 py-3 text-sm text-faint">No hosts.</div>}
      </Card>
      <button
        type="button"
        className={cn(btn, 'mt-3')}
        onClick={() => useStore.setState({ showConnect: true })}
      >
        Connect a host…
      </button>
    </Section>
  )
}

function ProjectsPage() {
  const hostOrder = useStore((s) => s.hostOrder)
  const hosts = useStore((s) => s.hosts)
  const repos = useStore((s) => s.repos)
  const steps = usePrefs((s) => s.createSteps)
  const [open, setOpen] = useState<string | null>(null)
  return (
    <Section title="Repositories and folders on each host, their saved on-create steps and worktree setup">
      <Card>
        {hostOrder.flatMap((h) =>
          (repos[h] ?? []).map((r) => {
            const saved = steps[`${h}\u0000${r.id}`]
            const key = `${h}:${r.id}`
            const templates = r.kind !== 'folder' && hasWorktreeTemplates(h)
            return (
              <div key={key}>
                <div className="flex items-center gap-3 px-1 py-3">
                  <ProjectTile name={r.name} />
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium text-foreground">
                      {r.name}{' '}
                      <span className="font-normal text-faint">· {hosts[h]?.label ?? h}</span>
                    </div>
                    <div className="truncate font-mono text-xs text-faint">
                      {r.path} ·{' '}
                      {r.kind === 'folder' ? 'folder' : `default ${r.defaultBranch ?? 'HEAD'}`}
                    </div>
                  </div>
                  {r.kind === 'folder' ? (
                    <div className="text-xs text-muted-foreground">no git</div>
                  ) : (
                    <div
                      className="max-w-[220px] truncate font-mono text-xs text-muted-foreground"
                      title="Saved on-create steps"
                    >
                      {saved
                        ? saved.map((s) => s.cmd || s.kind).join(' → ') || 'no steps'
                        : 'default steps'}
                    </div>
                  )}
                  {templates && (
                    <button
                      type="button"
                      className={btn}
                      aria-expanded={open === key}
                      onClick={() => setOpen(open === key ? null : key)}
                    >
                      Worktree setup…
                    </button>
                  )}
                </div>
                {templates && open === key && <WorktreeTemplateEditor hostId={h} repo={r.id} />}
              </div>
            )
          })
        )}
      </Card>
    </Section>
  )
}
