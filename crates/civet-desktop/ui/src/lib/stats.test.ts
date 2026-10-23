import { describe, expect, it } from 'vitest'
import type { HostStats, ProcessUsage, StatsSample } from '@/types'
import {
  formatBytes,
  formatPermille,
  formatRate,
  formatUptime,
  groupByWorktree,
  MAX_SAMPLES,
  mergeStats,
  newestSeq,
  warnings
} from './stats'

const GiB = 2 ** 30

function sample(seq: number, over: Partial<StatsSample> = {}): StatsSample {
  return {
    seq,
    atMs: seq * 2000,
    cpu: 100,
    memUsed: 10 * GiB,
    swapUsed: 0,
    netRx: 0,
    netTx: 0,
    diskRead: 0,
    diskWrite: 0,
    gpus: [],
    ...over
  }
}

function stats(seqs: number[], over: Partial<HostStats> = {}): HostStats {
  return {
    intervalMs: 2000,
    samples: seqs.map((s) => sample(s)),
    cores: [],
    memTotal: 64 * GiB,
    swapTotal: 0,
    load: [0, 0, 0],
    uptimeSecs: 0,
    disks: [],
    gpus: [],
    processes: [],
    ...over
  }
}

const seqs = (s: HostStats) => s.samples.map((x) => x.seq)

describe('mergeStats', () => {
  it('appends samples that follow on', () => {
    expect(seqs(mergeStats(stats([1, 2, 3]), stats([4, 5])))).toEqual([1, 2, 3, 4, 5])
  })

  it('keeps the history when a poll brings nothing new', () => {
    const m = mergeStats(stats([1, 2]), stats([], { cores: [500] }))
    expect(seqs(m)).toEqual([1, 2])
    expect(m.cores).toEqual([500])
  })

  it('starts over after a gap or a daemon restart', () => {
    expect(seqs(mergeStats(stats([1, 2]), stats([7, 8, 9])))).toEqual([7, 8, 9])
    expect(seqs(mergeStats(stats([400, 401]), stats([0, 1])))).toEqual([0, 1])
    expect(seqs(mergeStats(null, stats([3])))).toEqual([3])
  })

  it('caps the history', () => {
    const many = Array.from({ length: MAX_SAMPLES }, (_, i) => i)
    const m = mergeStats(stats(many), stats([MAX_SAMPLES, MAX_SAMPLES + 1]))
    expect(m.samples).toHaveLength(MAX_SAMPLES)
    expect(newestSeq(m)).toBe(MAX_SAMPLES + 1)
    expect(newestSeq(null)).toBeNull()
  })
})

describe('formatting', () => {
  it('formats bytes, rates, percentages and uptime', () => {
    expect(formatBytes(512)).toBe('512B')
    expect(formatBytes(41.25 * GiB)).toBe('41.3G')
    expect(formatBytes(512 * GiB)).toBe('512G')
    expect(formatBytes(1024 * GiB)).toBe('1.0T')
    expect(formatRate(0)).toBe('0 B/s')
    expect(formatRate(12_340_000)).toBe('12.3 MB/s')
    expect(formatRate(250_000)).toBe('250 KB/s')
    expect(formatPermille(625)).toBe('63%')
    expect(formatUptime(93_784)).toBe('1d 2h')
    expect(formatUptime(7_260)).toBe('2h 1m')
    expect(formatUptime(300)).toBe('5m')
  })
})

describe('warnings', () => {
  it('flags full memory and disks, worst first', () => {
    const s = stats([1], {
      disks: [
        { mount: '/', total: 100, available: 30 },
        { mount: '/data', total: 100, available: 4 }
      ]
    })
    s.samples[0]!.memUsed = 0.92 * 64 * GiB
    expect(warnings(s)).toEqual(['disk /data 96%', 'mem 92%'])
    expect(warnings(stats([1]))).toEqual([])
  })
})

describe('groupByWorktree', () => {
  const proc = (
    pid: number,
    cpu: number,
    pane: number | null,
    worktree: string | null
  ): ProcessUsage => ({
    pid,
    name: `p${pid}`,
    command: `p${pid}`,
    cpu,
    mem: 100,
    pane,
    worktree
  })

  it('sums by worktree, busiest first, other processes last', () => {
    const groups = groupByWorktree([
      proc(1, 50, 7, '/w/a'),
      proc(2, 3000, 7, '/w/a'),
      proc(3, 400, 9, '/w/b'),
      proc(4, 10, 8, '/w/a'),
      proc(5, 900, null, null),
      proc(6, 1, null, null)
    ])
    expect(groups.map((g) => [g.worktree, g.cpu, g.mem])).toEqual([
      ['/w/a', 3060, 300],
      ['/w/b', 400, 100],
      [null, 901, 200]
    ])
    expect(groups[0]!.panes).toEqual([7, 8])
    expect(groups[0]!.processes.map((p) => p.pid)).toEqual([2, 1, 4])
    expect(groups[2]!.panes).toEqual([])
  })

  it('has no other group when every process is in a pane', () => {
    expect(groupByWorktree([proc(1, 5, 1, '/w')]).map((g) => g.worktree)).toEqual(['/w'])
    expect(groupByWorktree([])).toEqual([])
  })
})
