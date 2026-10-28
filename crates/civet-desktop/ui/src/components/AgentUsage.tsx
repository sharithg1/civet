// Agent token use and estimated spend (state/usage.ts): the stats view's
// usage panel (spend today / 7 / 30 days, a bar per day, by project, by
// worktree and the recent sessions) and the sidebar's per-worktree figure.
// Every amount is an estimate at the daemon's list prices, and says so.
// The daily chart is one series in the chart hue, like the other stats.

import { useMemo, useState } from 'react'
import { AlertTriangle } from 'lucide-react'
import { cn } from '@/lib/utils'
import {
  byProject,
  byWorktree,
  dailyUsage,
  formatTokens,
  formatUsd,
  modelLabel,
  startOfLocalDay,
  totals,
  totalTokens,
  type UsageDay,
  type UsageRow
} from '@/lib/usage'
import { ago, compactWorktreeName, worktreeBranch } from '@/lib/worktree-view'
import { usePrefs } from '@/state/prefs'
import { hasAgentUsage, USAGE_DAYS, useAgentUsage } from '@/state/usage'
import { useStore, wtKey } from '@/state/store'
import type { AgentSessionUsage, AgentUsage, TokenCounts, WorktreeInfo } from '@/types'
import { AgentLogo } from './AgentLogo'

const DAY_MS = 86_400_000

/** Recent sessions listed in the panel. */
const MAX_SESSIONS = 12

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

/**
 * A worktree's estimated agent spend over the usage window, for its sidebar
 * row. Nothing when there is none (or Settings hides it).
 */
export function WorktreeSpend({ hostId, worktree }: { hostId: string; worktree: string }) {
  const show = usePrefs((p) => p.sidebarSpend)
  const usage = useAgentUsage((s) => s.hosts[hostId]?.usage)
  const t = useMemo(
    () => (usage ? totals(usage.sessions.filter((s) => s.worktree === worktree)) : null),
    [usage, worktree]
  )
  if (!show || !t || t.costMicros < 10_000) return null
  return (
    <span
      className="shrink-0 font-mono text-xs text-dim tabular-nums"
      title={[
        `Agent spend, last ${USAGE_DAYS} days: ${formatUsd(t.costMicros)} (estimate at list prices)`,
        `${formatTokens(totalTokens(t.tokens))} tokens · ${t.sessions} session${t.sessions === 1 ? '' : 's'}`
      ].join('\n')}
    >
      {formatUsd(t.costMicros)}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Stats view panel
// ---------------------------------------------------------------------------

export function AgentUsagePanel({
  hostId,
  activeWorktree
}: {
  hostId: string
  activeWorktree: string
}) {
  const host = useStore((s) => s.hosts[hostId])
  const view = useAgentUsage((s) => s.hosts[hostId])
  const usage = view?.usage
  return (
    <section className="flex flex-col gap-3 rounded-lg border border-border-subtle bg-card/40 p-3">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <h2 className="text-xs font-medium text-quiet">Agent usage</h2>
        <span className="flex items-center gap-2 text-xs text-dim">
          {view?.error && (
            <span className="flex items-center gap-1 text-status-err-text" title={view.error}>
              <AlertTriangle className="size-3" aria-hidden />
              Not updating
            </span>
          )}
          {usage && <PricesNote usage={usage} />}
        </span>
      </div>
      {!usage ? (
        <div className="py-2 text-xs text-faint">
          {hasAgentUsage(host)
            ? view?.error
              ? view.error
              : 'Reading agent logs…'
            : `Agent usage needs civetd 0.4.4 or newer on ${host?.label ?? hostId}`}
        </div>
      ) : usage.sessions.length === 0 ? (
        <div className="py-2 text-xs text-faint">
          No Claude Code or Codex sessions in the last {USAGE_DAYS} days.
        </div>
      ) : (
        <UsageBody hostId={hostId} usage={usage} activeWorktree={activeWorktree} />
      )}
    </section>
  )
}

/** "Estimates at list prices": the table on hover. */
function PricesNote({ usage }: { usage: AgentUsage }) {
  const per = (micros: number) => `$${(micros / 1e6).toFixed(micros < 1e6 ? 3 : 2)}`
  const rows = usage.prices.map(
    (p) =>
      `${p.model}: in ${per(p.input)} · out ${per(p.output)} · cache write ${per(p.cacheWrite5m)}/${per(p.cacheWrite1h)} (5m/1h) · read ${per(p.cacheRead)}`
  )
  return (
    <span
      className="cursor-help underline decoration-dotted underline-offset-2"
      title={[
        `Estimated from the agents' token counts at API list prices per million tokens (as of ${usage.pricesAsOf}). Subscriptions and discounts are not known.`,
        '',
        ...rows
      ].join('\n')}
    >
      Estimates at list prices
    </span>
  )
}

function UsageBody({
  hostId,
  usage,
  activeWorktree
}: {
  hostId: string
  usage: AgentUsage
  activeWorktree: string
}) {
  const now = usage.scannedAtMs
  const today = startOfLocalDay(now)
  const sum = useMemo(() => {
    const at = (days: number) => totals(usage.sessions, today - (days - 1) * DAY_MS)
    return { today: at(1), week: at(7), month: totals(usage.sessions) }
  }, [usage, today])
  const days = useMemo(() => dailyUsage(usage.sessions, USAGE_DAYS, now), [usage, now])
  const projects = useMemo(() => byProject(usage.sessions), [usage])
  const worktrees = useMemo(() => byWorktree(usage.sessions), [usage])
  const info = useWorktreeInfos(hostId)
  const repos = useStore((s) => s.repos[hostId])
  const repoName = (id: string | null) =>
    id == null ? 'Outside projects' : (repos?.find((r) => r.id === id)?.name ?? id)
  const wtName = (id: string | null) => {
    if (id == null) return 'Outside worktrees'
    const wt = info.get(id)
    return wt ? compactWorktreeName(worktreeBranch(wt), 32) : (id.split('/').pop() ?? id)
  }
  return (
    <>
      <div className="grid grid-cols-3 gap-2">
        <Tile label="Today" t={sum.today} />
        <Tile label="7 days" t={sum.week} />
        <Tile label={`${USAGE_DAYS} days`} t={sum.month} />
      </div>
      <DailyChart days={days} />
      {sum.month.unpricedTokens > 0 && (
        <div className="text-xs text-faint">
          {formatTokens(sum.month.unpricedTokens)} tokens from models without a known price are not
          in these amounts.
        </div>
      )}
      <div className="grid grid-cols-1 gap-3 xl:grid-cols-2">
        <UsageTable title="By project" rows={projects} name={repoName} isActive={() => false} />
        <UsageTable
          title="By worktree"
          rows={worktrees}
          name={wtName}
          tooltip={(k) => k ?? 'Sessions whose directory is in no worktree of a project'}
          isActive={(k) => k === activeWorktree}
        />
      </div>
      <Sessions sessions={usage.sessions} wtName={wtName} now={now} />
    </>
  )
}

/** Worktree id → info, across the host's projects. */
export function useWorktreeInfos(hostId: string): Map<string, WorktreeInfo> {
  const repos = useStore((s) => s.repos[hostId])
  const worktrees = useStore((s) => s.worktrees)
  return useMemo(() => {
    const m = new Map<string, WorktreeInfo>()
    for (const r of repos ?? [])
      for (const w of worktrees[wtKey(hostId, r.id)] ?? []) m.set(w.id, w)
    return m
  }, [hostId, repos, worktrees])
}

function tokenBreakdown(t: TokenCounts): string {
  return [
    `Input ${formatTokens(t.input)}`,
    `Output ${formatTokens(t.output)}`,
    `Cache write ${formatTokens(t.cacheWrite)}`,
    `Cache read ${formatTokens(t.cacheRead)}`
  ].join(' · ')
}

function Tile({ label, t }: { label: string; t: ReturnType<typeof totals> }) {
  return (
    <div
      className="flex min-w-0 flex-col gap-0.5 rounded-[5px] bg-field px-2.5 py-2"
      title={tokenBreakdown(t.tokens)}
    >
      <span className="text-xs text-faint">{label}</span>
      <span className="font-mono text-lg text-foreground tabular-nums">
        {formatUsd(t.costMicros)}
      </span>
      <span className="truncate font-mono text-xs text-quiet tabular-nums">
        {formatTokens(totalTokens(t.tokens))} tokens · {t.sessions} session
        {t.sessions === 1 ? '' : 's'}
      </span>
    </div>
  )
}

/** Estimated spend per local day, newest at the right, with a hover readout. */
function DailyChart({ days }: { days: readonly UsageDay[] }) {
  const [hover, setHover] = useState<number | null>(null)
  const max = Math.max(1, ...days.map((d) => d.costMicros))
  const hovered = hover != null ? days[hover] : undefined
  const fmtDay = (ms: number) =>
    new Date(ms).toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between text-xs">
        <span className="text-faint">Per day</span>
        <span className="font-mono text-soft tabular-nums">
          {hovered
            ? `${fmtDay(hovered.dayMs)} · ${formatUsd(hovered.costMicros)} · ${formatTokens(hovered.tokens)} tokens`
            : `max ${formatUsd(max)}`}
        </span>
      </div>
      <div
        role="img"
        aria-label={`Estimated agent spend per day, last ${days.length} days`}
        className="flex h-16 items-end gap-0.5 rounded-[5px] bg-field px-1 pt-1"
        onPointerLeave={() => setHover(null)}
      >
        {days.map((d, i) => (
          <div
            key={d.dayMs}
            className="flex h-full min-w-0 flex-1 items-end"
            onPointerEnter={() => setHover(i)}
          >
            <div
              className={cn(
                'w-full rounded-t-[3px]',
                hover === i ? 'bg-chart-2' : 'bg-chart-2/75',
                d.costMicros === 0 && 'bg-transparent'
              )}
              style={{ height: d.costMicros > 0 ? `max(2px, ${(d.costMicros / max) * 100}%)` : 0 }}
            />
          </div>
        ))}
      </div>
      <div className="flex justify-between text-xs text-dim">
        <span>{days[0] ? fmtDay(days[0].dayMs) : ''}</span>
        <span>Today</span>
      </div>
    </div>
  )
}

function UsageTable<K extends string | null>({
  title,
  rows,
  name,
  tooltip,
  isActive
}: {
  title: string
  rows: UsageRow<K>[]
  name: (k: K) => string
  tooltip?: (k: K) => string
  isActive: (k: K) => boolean
}) {
  return (
    <div className="flex min-w-0 flex-col text-xs">
      <div className="grid grid-cols-[minmax(0,1fr)_64px_64px_40px] gap-2 border-b border-border-subtle px-1.5 py-1 text-dim">
        <span>{title}</span>
        <span className="text-right">Spend</span>
        <span className="text-right">Tokens</span>
        <span className="text-right" title="Sessions">
          Sess.
        </span>
      </div>
      {rows.map((r) => (
        <div
          key={r.key ?? ''}
          title={`${tooltip ? tooltip(r.key) : name(r.key)}\n${tokenBreakdown(r.tokens)}`}
          className={cn(
            'grid grid-cols-[minmax(0,1fr)_64px_64px_40px] gap-2 rounded-[5px] px-1.5 py-1',
            isActive(r.key) && 'bg-hover'
          )}
        >
          <span
            className={cn(
              'flex min-w-0 items-center gap-1.5 truncate font-mono',
              r.key == null ? 'text-soft' : 'text-foreground'
            )}
          >
            <span className="truncate">{name(r.key)}</span>
            {isActive(r.key) && <span className="shrink-0 font-sans text-dim">this worktree</span>}
          </span>
          <span className="text-right font-mono text-soft tabular-nums">
            {formatUsd(r.costMicros)}
          </span>
          <span className="text-right font-mono text-quiet tabular-nums">
            {formatTokens(totalTokens(r.tokens))}
          </span>
          <span className="text-right font-mono text-faint tabular-nums">{r.sessions}</span>
        </div>
      ))}
    </div>
  )
}

function Sessions({
  sessions,
  wtName,
  now
}: {
  sessions: readonly AgentSessionUsage[]
  wtName: (id: string | null) => string
  now: number
}) {
  const shown = sessions.slice(0, MAX_SESSIONS)
  return (
    <div className="flex flex-col text-xs">
      <div className="grid grid-cols-[minmax(0,1fr)_96px_64px_64px_44px] gap-2 border-b border-border-subtle px-1.5 py-1 text-dim">
        <span>Recent sessions</span>
        <span>Model</span>
        <span className="text-right">Spend</span>
        <span className="text-right">Tokens</span>
        <span className="text-right">Last</span>
      </div>
      {shown.map((s) => (
        <div
          key={`${s.agent}:${s.sessionId}`}
          title={`${s.agent === 'claude' ? 'Claude Code' : 'Codex'} session ${s.sessionId}\n${s.cwd}\n${s.models.join(', ')}\n${tokenBreakdown(s.tokens)}`}
          className="grid grid-cols-[minmax(0,1fr)_96px_64px_64px_44px] items-center gap-2 px-1.5 py-1"
        >
          <span className="flex min-w-0 items-center gap-1.5">
            <AgentLogo agent={s.agent} className="size-3" />
            <span
              className={cn('truncate font-mono', s.worktree ? 'text-foreground' : 'text-soft')}
            >
              {s.worktree ? wtName(s.worktree) : s.cwd}
            </span>
          </span>
          <span className="truncate text-quiet">
            {s.models[0] ? modelLabel(s.models[0]) : '–'}
            {s.models.length > 1 && <span className="text-dim"> +{s.models.length - 1}</span>}
          </span>
          <span className="text-right font-mono text-soft tabular-nums">
            {s.unpricedTokens > 0 && s.costMicros === 0 ? (
              <span className="text-dim" title="No price for this model">
                –
              </span>
            ) : (
              formatUsd(s.costMicros)
            )}
          </span>
          <span className="text-right font-mono text-quiet tabular-nums">
            {formatTokens(totalTokens(s.tokens))}
          </span>
          <span className="text-right text-faint tabular-nums">{ago(s.lastMs, now)}</span>
        </div>
      ))}
      {sessions.length > shown.length && (
        <div className="px-1.5 pt-1 text-dim">
          and {sessions.length - shown.length} more in the last {USAGE_DAYS} days
        </div>
      )}
    </div>
  )
}
