// Pure helpers for host stats (state/stats.ts, StatsView, the title bar's
// stats indicator): merging polled samples into a history, formatting, and
// grouping processes by worktree.

import type { HostStats, PaneId, ProcessUsage, StatsSample, WorktreeId } from '@/types'

/** Samples kept per host (the daemon's 10 minutes at 2 s). */
export const MAX_SAMPLES = 300

/** Memory / disk use from which the indicator warns. */
export const WARN_FRACTION = 0.9

/**
 * Fold a poll into what we had: its samples continue the history when they
 * follow on from it, else (daemon restarted, or we were away longer than its
 * history) they replace it.
 */
export function mergeStats(prev: HostStats | null, next: HostStats): HostStats {
  const had = prev?.samples ?? []
  const last = had.at(-1)?.seq
  const first = next.samples[0]?.seq
  let samples: StatsSample[]
  if (first == null) samples = had
  else if (last != null && first === last + 1) samples = [...had, ...next.samples]
  else samples = next.samples
  return { ...next, samples: samples.slice(-MAX_SAMPLES) }
}

/** Newest seq we have, for the next poll's `since`. */
export function newestSeq(s: HostStats | null): number | null {
  return s?.samples.at(-1)?.seq ?? null
}

/** Binary units, short: 512B, 3.4K, 41.2G, 1.0T. */
export function formatBytes(n: number): string {
  const units = ['B', 'K', 'M', 'G', 'T', 'P']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  if (i === 0) return `${Math.round(v)}B`
  return `${v >= 100 ? Math.round(v) : v.toFixed(1)}${units[i]}`
}

/** Bytes per second: 0 B/s, 12.3 MB/s (decimal, like network tools). */
export function formatRate(n: number): string {
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let v = n
  let i = 0
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000
    i++
  }
  return `${i === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

/** Permille → "62%". */
export function formatPermille(p: number): string {
  return `${Math.round(p / 10)}%`
}

/** 93784 → "1d 2h", 7260 → "2h 1m", 300 → "5m". */
export function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400)
  const h = Math.floor((secs % 86400) / 3600)
  const m = Math.floor((secs % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${m}m`
  return `${m}m`
}

/** Something about to run out, worst first, for the indicator: "mem 93%", "disk / 95%". */
export function warnings(s: HostStats): string[] {
  const out: { frac: number; text: string }[] = []
  const latest = s.samples.at(-1)
  if (latest && s.memTotal > 0) {
    const frac = latest.memUsed / s.memTotal
    if (frac >= WARN_FRACTION) out.push({ frac, text: `mem ${Math.round(frac * 100)}%` })
  }
  for (const d of s.disks) {
    if (d.total <= 0) continue
    const frac = 1 - d.available / d.total
    if (frac >= WARN_FRACTION)
      out.push({ frac, text: `disk ${d.mount} ${Math.round(frac * 100)}%` })
  }
  return out.sort((a, b) => b.frac - a.frac).map((w) => w.text)
}

/** What one worktree's processes use (or the host's other processes). */
export type UsageGroup = {
  /** null: processes in no pane. */
  worktree: WorktreeId | null
  /** Permille of one core, summed. */
  cpu: number
  mem: number
  /** Panes with processes here, busiest first. */
  panes: PaneId[]
  /** Busiest first. */
  processes: ProcessUsage[]
}

/**
 * Processes by worktree, busiest first, then everything in no worktree
 * (other processes, and the rare pane without one) as the last group.
 */
export function groupByWorktree(processes: readonly ProcessUsage[]): UsageGroup[] {
  const groups = new Map<string, UsageGroup>()
  const other: UsageGroup = { worktree: null, cpu: 0, mem: 0, panes: [], processes: [] }
  const paneCpu = new Map<PaneId, number>()
  for (const p of processes) {
    const g =
      p.worktree != null
        ? (groups.get(p.worktree) ??
          groups
            .set(p.worktree, { worktree: p.worktree, cpu: 0, mem: 0, panes: [], processes: [] })
            .get(p.worktree)!)
        : other
    g.cpu += p.cpu
    g.mem += p.mem
    g.processes.push(p)
    if (p.pane != null && g !== other) {
      if (!g.panes.includes(p.pane)) g.panes.push(p.pane)
      paneCpu.set(p.pane, (paneCpu.get(p.pane) ?? 0) + p.cpu)
    }
  }
  const busiest = (a: ProcessUsage, b: ProcessUsage) => b.cpu - a.cpu || b.mem - a.mem
  const out = [...groups.values()].sort((a, b) => b.cpu - a.cpu || b.mem - a.mem)
  for (const g of out) {
    g.processes.sort(busiest)
    g.panes.sort((a, b) => (paneCpu.get(b) ?? 0) - (paneCpu.get(a) ?? 0))
  }
  other.processes.sort(busiest)
  if (other.processes.length) out.push(other)
  return out
}
