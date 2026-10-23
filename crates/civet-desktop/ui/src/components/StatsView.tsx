// btop-style host stats: the title bar's indicator (CPU sparkline, CPU %,
// memory, and a warning when memory or a disk is nearly full) and the view
// it opens over the work area: CPU (history and every core), memory,
// network, disk I/O, filesystems, GPUs, and what each worktree's processes
// use. Data comes from state/stats.ts (the active worktree's host). Every
// chart is one series in the chart hue; the status colors only mark
// trouble, always next to a label.

import { useMemo, useState, type PointerEvent as ReactPointerEvent } from 'react'
import { Activity, AlertTriangle, ChevronRight } from 'lucide-react'
import { cn } from '@/lib/utils'
import {
  formatBytes,
  formatPermille,
  formatRate,
  formatUptime,
  groupByWorktree,
  MAX_SAMPLES,
  WARN_FRACTION,
  warnings,
  type UsageGroup
} from '@/lib/stats'
import { agentName, compactWorktreeName, paneWhat, worktreeBranch } from '@/lib/worktree-view'
import { usePrefs } from '@/state/prefs'
import { hasHostStats, useHostStats } from '@/state/stats'
import { useStore, wtKey } from '@/state/store'
import type { HostStats, PaneInfo, ProcessUsage, StatsSample, WorktreeInfo } from '@/types'

/** Samples in the indicator's sparkline (one minute at 2 s). */
const INDICATOR_SAMPLES = 30

// ---------------------------------------------------------------------------
// Title bar indicator
// ---------------------------------------------------------------------------

/**
 * Right end of the title bar; toggles the stats view. Hidden until stats
 * arrive, and when Settings turns it off (`showHostStats`).
 */
export function StatsIndicator() {
  const hostId = useStore((s) => s.active?.hostId ?? null)
  const supported = useStore((s) => (hostId ? hasHostStats(s.hosts[hostId]) : false))
  const open = useStore((s) => s.statsOpen)
  const show = usePrefs((p) => p.showHostStats)
  const sparkline = usePrefs((p) => p.hostStatsSparkline)
  const stats = useHostStats((s) => (hostId ? s.hosts[hostId]?.stats : null))
  const latest = stats?.samples.at(-1)
  if (!show || !hostId || !supported || !stats || !latest) return null
  const warn = warnings(stats)
  const cpu = stats.samples.slice(-INDICATOR_SAMPLES).map((s) => s.cpu)
  const tooltip = [
    `CPU ${formatPermille(latest.cpu)} · load ${(stats.load[0] / 100).toFixed(2)}`,
    `Memory ${formatBytes(latest.memUsed)} of ${formatBytes(stats.memTotal)}`,
    ...warn.map((w) => `⚠ ${w}`),
    open ? 'Hide host stats' : 'Show host stats'
  ].join('\n')
  return (
    <button
      type="button"
      title={tooltip}
      aria-label="Host stats"
      aria-pressed={open}
      onClick={() => useStore.setState((s) => ({ statsOpen: !s.statsOpen }))}
      className={cn(
        'flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-md px-2 font-mono text-xs tabular-nums',
        open ? 'bg-card text-foreground' : 'text-quiet hover:bg-hover hover:text-soft'
      )}
    >
      {sparkline && (
        <Sparkline values={cpu} slots={INDICATOR_SAMPLES} max={1000} className="h-3.5 w-9" />
      )}
      <span className="w-8 text-right">{formatPermille(latest.cpu)}</span>
      <span className="text-dim">·</span>
      <span>{formatBytes(latest.memUsed)}</span>
      {warn[0] && (
        <span className="flex items-center gap-1 text-status-err-text">
          <AlertTriangle className="size-3" aria-hidden />
          {warn[0]}
        </span>
      )}
    </button>
  )
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

/**
 * Line + soft area, newest at the right edge; `slots` fixes the time span
 * so a short history fills from the right instead of stretching.
 */
function Sparkline({
  values,
  slots,
  max,
  className
}: {
  values: readonly number[]
  slots: number
  max: number
  className?: string
}) {
  const h = 100
  const top = Math.max(max, 1)
  const offset = slots - values.length
  const pts = values.map(
    (v, i) => [offset + i, h - (Math.min(v, top) / top) * (h - 2) - 1] as const
  )
  const line = pts.map(([x, y], i) => `${i ? 'L' : 'M'}${x},${y.toFixed(1)}`).join('')
  const area = pts.length ? `${line}L${pts.at(-1)![0]},${h}L${pts[0]![0]},${h}Z` : ''
  return (
    <svg
      viewBox={`0 0 ${Math.max(slots - 1, 1)} ${h}`}
      preserveAspectRatio="none"
      aria-hidden
      className={cn('shrink-0 overflow-visible text-chart-2', className)}
    >
      {pts.length > 1 && (
        <>
          <path d={area} fill="currentColor" fillOpacity={0.16} />
          <path
            d={line}
            fill="none"
            stroke="currentColor"
            strokeWidth={1.5}
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
        </>
      )}
    </svg>
  )
}

/** A history chart with a hover crosshair that reads out the value and its age. */
function HistoryChart({
  samples,
  value,
  max,
  format,
  label,
  height = 'h-16'
}: {
  samples: readonly StatsSample[]
  value: (s: StatsSample) => number
  /** null: scale to the largest value shown. */
  max: number | null
  format: (v: number) => string
  label: string
  height?: string
}) {
  const [hover, setHover] = useState<number | null>(null)
  const values = useMemo(() => samples.map(value), [samples, value])
  const top = max ?? Math.max(1, ...values) * 1.15
  const offset = MAX_SAMPLES - values.length
  const onMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect()
    const slot = Math.round(((e.clientX - r.left) / r.width) * (MAX_SAMPLES - 1))
    const i = slot - offset
    setHover(i >= 0 && i < values.length ? i : null)
  }
  const hovered = hover != null ? samples[hover] : undefined
  const newest = samples.at(-1)
  const ageSecs = hovered && newest ? Math.round((newest.atMs - hovered.atMs) / 1000) : 0
  return (
    <div
      role="img"
      aria-label={`${label}, last ${Math.round((samples.length * 2) / 60)} minutes`}
      className={cn('relative w-full rounded-[5px] bg-field', height)}
      onPointerMove={onMove}
      onPointerLeave={() => setHover(null)}
    >
      <Sparkline
        values={values}
        slots={MAX_SAMPLES}
        max={top}
        className="absolute inset-0 size-full"
      />
      {hovered && hover != null && (
        <>
          <div
            className="pointer-events-none absolute inset-y-0 w-px bg-border-strong"
            style={{ left: `${((offset + hover) / (MAX_SAMPLES - 1)) * 100}%` }}
          />
          <div
            className={cn(
              'pointer-events-none absolute -top-7 z-10 rounded-[5px] border border-border bg-popover px-1.5 py-0.5 text-xs whitespace-nowrap text-popover-foreground shadow-menu',
              (offset + hover) / MAX_SAMPLES > 0.6 ? '-translate-x-full' : ''
            )}
            style={{ left: `${((offset + hover) / (MAX_SAMPLES - 1)) * 100}%` }}
          >
            <span className="font-mono tabular-nums">{format(values[hover]!)}</span>
            <span className="text-faint">
              {' '}
              · {ageSecs < 2 ? 'now' : `${formatAge(ageSecs)} ago`}
            </span>
          </div>
        </>
      )}
    </div>
  )
}

function formatAge(secs: number): string {
  if (secs < 60) return `${secs}s`
  const m = Math.floor(secs / 60)
  const s = secs % 60
  return s ? `${m}m ${s}s` : `${m}m`
}

/** Horizontal fill meter; the status color (and an icon) from `WARN_FRACTION`. */
function Meter({ fraction, label }: { fraction: number; label: string }) {
  const f = Math.max(0, Math.min(1, fraction))
  const warn = f >= WARN_FRACTION
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(f * 100)}
      className="h-1.5 w-full overflow-hidden rounded-full bg-field"
    >
      <div
        className={cn('h-full rounded-full', warn ? 'bg-status-err' : 'bg-chart-2')}
        style={{ width: `${f * 100}%` }}
      />
    </div>
  )
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

export function StatsView({ hostId, worktree }: { hostId: string; worktree: string }) {
  const view = useHostStats((s) => s.hosts[hostId])
  const host = useStore((s) => s.hosts[hostId])
  const stats = view?.stats
  const latest = stats?.samples.at(-1)
  if (!stats || !latest) {
    return (
      <div className="grid h-full place-items-center text-sm text-quiet">
        {hasHostStats(host)
          ? 'Measuring…'
          : `Host stats need civetd 0.2.1 or newer on ${host?.label ?? hostId}`}
      </div>
    )
  }
  return (
    <div className="h-full overflow-y-auto">
      <div className="flex flex-col gap-3 px-4 pt-2 pb-6">
        <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-xs text-faint">
          <span className="flex items-center gap-1.5 text-sm text-foreground">
            <Activity className="size-3.5 text-quiet" aria-hidden />
            {host?.label ?? hostId}
          </span>
          <span>up {formatUptime(stats.uptimeSecs)}</span>
          <span className="font-mono tabular-nums">
            load {stats.load.map((l) => (l / 100).toFixed(2)).join(' ')}
          </span>
          <span>
            {stats.cores.length} cores · {formatBytes(stats.memTotal)}
          </span>
          {view.error && (
            <span className="flex items-center gap-1 text-status-err-text" title={view.error}>
              <AlertTriangle className="size-3" aria-hidden />
              Not updating
            </span>
          )}
        </header>

        <div className="grid grid-cols-1 gap-3 xl:grid-cols-2">
          <CpuPanel stats={stats} latest={latest} />
          <MemoryPanel stats={stats} latest={latest} />
          <RatePanel
            title="Network"
            samples={stats.samples}
            series={[
              { label: 'Received', value: (s) => s.netRx },
              { label: 'Sent', value: (s) => s.netTx }
            ]}
          />
          <RatePanel
            title="Disk I/O"
            samples={stats.samples}
            series={[
              { label: 'Read', value: (s) => s.diskRead },
              { label: 'Written', value: (s) => s.diskWrite }
            ]}
          />
          <DisksPanel stats={stats} />
          {stats.gpus.length > 0 && <GpuPanel stats={stats} />}
        </div>

        <ProcessesPanel hostId={hostId} activeWorktree={worktree} processes={stats.processes} />
      </div>
    </div>
  )
}

function Panel({
  title,
  value,
  children
}: {
  title: string
  value?: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <section className="flex min-w-0 flex-col gap-2.5 rounded-lg border border-border-subtle bg-card/40 p-3">
      <div className="flex items-baseline justify-between gap-2">
        <h2 className="text-xs font-medium text-quiet">{title}</h2>
        {value != null && (
          <div className="font-mono text-sm text-foreground tabular-nums">{value}</div>
        )}
      </div>
      {children}
    </section>
  )
}

const cpuOf = (s: StatsSample) => s.cpu
const memOf = (s: StatsSample) => s.memUsed

function CpuPanel({ stats, latest }: { stats: HostStats; latest: StatsSample }) {
  return (
    <Panel title="CPU" value={formatPermille(latest.cpu)}>
      <HistoryChart
        samples={stats.samples}
        value={cpuOf}
        max={1000}
        format={formatPermille}
        label="CPU use"
      />
      {/* One bar per core; dense hosts get hairline bars. */}
      <div className="flex h-8 items-end gap-px" aria-label="Per-core use">
        {stats.cores.map((c, i) => (
          <div
            key={i}
            title={`Core ${i}: ${formatPermille(c)}`}
            className="flex h-full min-w-px flex-1 items-end rounded-[2px] bg-field"
          >
            <div className="w-full rounded-[2px] bg-chart-2" style={{ height: `${c / 10}%` }} />
          </div>
        ))}
      </div>
    </Panel>
  )
}

function MemoryPanel({ stats, latest }: { stats: HostStats; latest: StatsSample }) {
  return (
    <Panel
      title="Memory"
      value={
        <>
          {formatBytes(latest.memUsed)}
          <span className="text-faint"> / {formatBytes(stats.memTotal)}</span>
        </>
      }
    >
      <HistoryChart
        samples={stats.samples}
        value={memOf}
        max={stats.memTotal}
        format={formatBytes}
        label="Memory in use"
      />
      <LabeledMeter label="Memory" used={latest.memUsed} total={stats.memTotal} />
      {stats.swapTotal > 0 && (
        <LabeledMeter label="Swap" used={latest.swapUsed} total={stats.swapTotal} />
      )}
    </Panel>
  )
}

function LabeledMeter({ label, used, total }: { label: string; used: number; total: number }) {
  const f = total > 0 ? used / total : 0
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className="min-w-0 truncate font-mono text-soft">{label}</span>
        <span className="flex shrink-0 items-center gap-1 font-mono text-faint tabular-nums">
          {f >= WARN_FRACTION && (
            <AlertTriangle className="size-3 text-status-err-text" aria-label="Nearly full" />
          )}
          {formatBytes(used)} / {formatBytes(total)}
          <span className="w-9 text-right text-soft">{Math.round(f * 100)}%</span>
        </span>
      </div>
      <Meter fraction={f} label={`${label} ${Math.round(f * 100)}% used`} />
    </div>
  )
}

/** Two rates as small multiples on one shared scale (never two axes). */
function RatePanel({
  title,
  samples,
  series
}: {
  title: string
  samples: readonly StatsSample[]
  series: { label: string; value: (s: StatsSample) => number }[]
}) {
  const latest = samples.at(-1)
  const max = useMemo(
    () => Math.max(1000, ...series.flatMap((x) => samples.map(x.value))) * 1.15,
    [samples, series]
  )
  return (
    <Panel title={title}>
      {series.map((x) => (
        <div key={x.label} className="flex flex-col gap-1">
          <div className="flex items-baseline justify-between text-xs">
            <span className="text-faint">{x.label}</span>
            <span className="font-mono text-soft tabular-nums">
              {latest ? formatRate(x.value(latest)) : '–'}
            </span>
          </div>
          <HistoryChart
            samples={samples}
            value={x.value}
            max={max}
            format={formatRate}
            label={`${title} ${x.label.toLowerCase()}`}
            height="h-10"
          />
        </div>
      ))}
    </Panel>
  )
}

function DisksPanel({ stats }: { stats: HostStats }) {
  return (
    <Panel title="Disks">
      {stats.disks.length === 0 && <div className="text-xs text-faint">No local disks found.</div>}
      {stats.disks.map((d) => (
        <LabeledMeter key={d.mount} label={d.mount} used={d.total - d.available} total={d.total} />
      ))}
    </Panel>
  )
}

function GpuPanel({ stats }: { stats: HostStats }) {
  const latest = stats.samples.at(-1)
  return (
    <Panel title={stats.gpus.length > 1 ? 'GPUs' : 'GPU'}>
      {stats.gpus.map((g, i) => (
        <div key={i} className="flex flex-col gap-1.5">
          <div className="flex items-baseline justify-between text-xs">
            <span className="min-w-0 truncate text-soft">
              {stats.gpus.length > 1 && <span className="text-faint">{i} · </span>}
              {g.name}
            </span>
            <span className="font-mono text-soft tabular-nums">
              {formatPermille(latest?.gpus[i] ?? 0)}
            </span>
          </div>
          <HistoryChart
            samples={stats.samples}
            value={(s) => s.gpus[i] ?? 0}
            max={1000}
            format={formatPermille}
            label={`GPU ${i} use`}
            height="h-10"
          />
          <LabeledMeter label="Memory" used={g.memUsed} total={g.memTotal} />
        </div>
      ))}
    </Panel>
  )
}

// ---------------------------------------------------------------------------
// Processes by worktree
// ---------------------------------------------------------------------------

function ProcessesPanel({
  hostId,
  activeWorktree,
  processes
}: {
  hostId: string
  activeWorktree: string
  processes: readonly ProcessUsage[]
}) {
  const groups = useMemo(() => groupByWorktree(processes), [processes])
  const info = useWorktreeInfos(hostId)
  const panes = useStore((s) => s.panes[hostId])
  const [toggled, setToggled] = useState<Record<string, boolean>>({})
  // The active worktree starts expanded, the rest collapsed.
  const isOpen = (key: string) => toggled[key] ?? key === activeWorktree
  return (
    <section className="flex flex-col gap-1 rounded-lg border border-border-subtle bg-card/40 p-3">
      <div className="flex items-baseline justify-between gap-2">
        <h2 className="text-xs font-medium text-quiet">By worktree</h2>
        <span className="text-xs text-dim">CPU in % of one core</span>
      </div>
      {groups.length === 0 ? (
        <div className="py-2 text-xs text-faint">Measuring processes…</div>
      ) : (
        <div className="text-xs">
          <div className="grid grid-cols-[minmax(0,1fr)_64px_72px_40px] gap-2 border-b border-border-subtle px-1.5 py-1 text-dim">
            <span>Worktree</span>
            <span className="text-right">CPU</span>
            <span className="text-right">Memory</span>
            <span className="text-right">Procs</span>
          </div>
          {groups.map((g) => {
            const key = g.worktree ?? ''
            const open = isOpen(key)
            return (
              <div key={key}>
                <GroupRow
                  group={g}
                  wt={g.worktree ? info.get(g.worktree) : undefined}
                  panes={panes}
                  active={g.worktree === activeWorktree}
                  open={open}
                  onToggle={() => setToggled((t) => ({ ...t, [key]: !open }))}
                />
                {open && g.processes.map((p) => <ProcessRow key={p.pid} p={p} />)}
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}

/** Worktree id → info, across the host's projects. */
function useWorktreeInfos(hostId: string): Map<string, WorktreeInfo> {
  const repos = useStore((s) => s.repos[hostId])
  const worktrees = useStore((s) => s.worktrees)
  return useMemo(() => {
    const m = new Map<string, WorktreeInfo>()
    for (const r of repos ?? [])
      for (const w of worktrees[wtKey(hostId, r.id)] ?? []) m.set(w.id, w)
    return m
  }, [hostId, repos, worktrees])
}

function GroupRow({
  group,
  wt,
  panes,
  active,
  open,
  onToggle
}: {
  group: UsageGroup
  wt: WorktreeInfo | undefined
  panes: Record<number, PaneInfo> | undefined
  active: boolean
  open: boolean
  onToggle: () => void
}) {
  const name = group.worktree
    ? wt
      ? compactWorktreeName(worktreeBranch(wt), 32)
      : (group.worktree.split('/').pop() ?? group.worktree)
    : 'Other processes'
  const kinds = paneKinds(group.panes.map((id) => panes?.[id]))
  const what = group.panes.map((id) => paneWhat(panes?.[id]).text).join('\n')
  return (
    <button
      type="button"
      aria-expanded={open}
      onClick={onToggle}
      title={group.worktree ?? 'Processes in no Civet pane'}
      className={cn(
        'grid w-full cursor-pointer grid-cols-[minmax(0,1fr)_64px_72px_40px] items-center gap-2 rounded-[5px] px-1.5 py-1.5 text-left hover:bg-hover',
        active && 'bg-hover'
      )}
    >
      <span className="flex min-w-0 items-center gap-1.5">
        <ChevronRight
          className={cn('size-3 shrink-0 text-dim transition-transform', open && 'rotate-90')}
          aria-hidden
        />
        <span
          className={cn('truncate font-mono', group.worktree ? 'text-foreground' : 'text-soft')}
        >
          {name}
        </span>
        {active && <span className="shrink-0 text-dim">this worktree</span>}
        {kinds && (
          <span className="min-w-0 truncate text-faint" title={what}>
            {kinds}
          </span>
        )}
      </span>
      <span className="text-right font-mono text-soft tabular-nums">
        {formatPermille(group.cpu)}
      </span>
      <span className="text-right font-mono text-soft tabular-nums">{formatBytes(group.mem)}</span>
      <span className="text-right font-mono text-faint tabular-nums">{group.processes.length}</span>
    </button>
  )
}

/** "Claude ×2 · Terminal": what runs in a worktree's panes, busiest kind first. */
function paneKinds(panes: readonly (PaneInfo | undefined)[]): string {
  const counts = new Map<string, number>()
  for (const p of panes) {
    const kind = p?.agent ? agentName(p) : 'Terminal'
    counts.set(kind, (counts.get(kind) ?? 0) + 1)
  }
  return [...counts].map(([k, n]) => (n > 1 ? `${k} ×${n}` : k)).join(' · ')
}

function ProcessRow({ p }: { p: ProcessUsage }) {
  return (
    <div
      title={p.command}
      className="grid grid-cols-[minmax(0,1fr)_64px_72px_40px] gap-2 px-1.5 py-0.5"
    >
      <span className="flex min-w-0 gap-2 pl-[18px] font-mono">
        <span className="w-12 shrink-0 text-right text-dim tabular-nums">{p.pid}</span>
        <span className="shrink-0 text-soft">{p.name}</span>
        <span className="min-w-0 truncate text-faint">{p.command}</span>
      </span>
      <span className="text-right font-mono text-quiet tabular-nums">{formatPermille(p.cpu)}</span>
      <span className="text-right font-mono text-quiet tabular-nums">{formatBytes(p.mem)}</span>
      <span />
    </div>
  )
}
