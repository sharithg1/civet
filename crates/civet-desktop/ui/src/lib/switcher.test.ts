import { describe, expect, it } from 'vitest'
import { FEW_WORKTREES, switcherGroups, type SwitcherEntry } from './switcher'

const entry = (name: string, p: Partial<SwitcherEntry> = {}): SwitcherEntry => ({
  key: `local\u0000/wt/${name}`,
  hostId: 'local',
  repo: 'civet',
  name,
  path: `/wt/${name.split('/').pop()}`,
  project: 'civet',
  host: 'This Mac',
  local: true,
  task: null,
  ...p
})
const keys = (gs: ReturnType<typeof switcherGroups>) => gs.map((g) => [g.label, g.rows.map((r) => r.name)])
const opts = (o: Partial<Parameters<typeof switcherGroups>[1]> = {}) => ({
  query: '',
  mru: [],
  currentKey: null,
  recentCount: 5,
  ...o
})

describe('switcher: empty query', () => {
  const a = entry('main')
  const b = entry('desktop/codex-tabs')
  const c = entry('fix-upload')
  it('lists current, then recent, then the rest, flat when there are few', () => {
    const gs = switcherGroups([a, b, c], opts({ mru: [c.key, b.key], currentKey: c.key }))
    expect(keys(gs)).toEqual([[null, ['fix-upload', 'desktop/codex-tabs', 'main']]])
    expect(gs[0]!.rows.map((r) => r.current)).toEqual([true, false, false])
  })

  it('groups the rest by project when there are many', () => {
    const many = Array.from({ length: FEW_WORKTREES }, (_, i) => entry(`b${i}`))
    const other = entry('x', { key: 'h\u0000/x', hostId: 'h', repo: 'api', project: 'api', host: 'devbox', local: false })
    const gs = switcherGroups([...many, other], opts({ mru: ['h\u0000/x', many[3]!.key], currentKey: 'h\u0000/x', recentCount: 1 }))
    expect(gs.map((g) => [g.label, g.host, g.rows.length])).toEqual([
      ['Recent', null, 2],
      ['civet', 'This Mac', FEW_WORKTREES - 1]
    ])
    expect(gs[0]!.rows.map((r) => r.name)).toEqual(['x', 'b3'])
  })
})

describe('switcher: search', () => {
  it('matches name, project, task and directory', () => {
    const t = entry('feat/a', { task: 'Fix the PDF export' })
    const d = entry('feat/b', { path: '/wt/pdf-dir' })
    const n = entry('other')
    const rows = switcherGroups([t, d, n], opts({ query: 'pdf' }))[0]!.rows
    expect(rows.map((r) => r.name).sort()).toEqual(['feat/a', 'feat/b'])
    expect(rows.find((r) => r.name === 'feat/a')!.hits.task.length).toBe(3)
  })

  it('ranks an exact name first, then recency on ties', () => {
    const exact = entry('team/tabs')
    const fuzzyHit = entry('tab-strip')
    const fuzzyOld = entry('tabular')
    const rows = switcherGroups([fuzzyOld, fuzzyHit, exact], opts({ query: 'tabs', mru: [fuzzyHit.key] }))[0]!.rows
    expect(rows[0]!.name).toBe('team/tabs')
    const tie = switcherGroups([entry('a-x'), entry('b-x')], opts({ query: 'x', mru: [entry('b-x').key] }))[0]!.rows
    expect(tie.map((r) => r.name)).toEqual(['b-x', 'a-x'])
  })

  it('groups by project only when several projects have several hits', () => {
    const api = (n: string) => entry(n, { key: `local\u0000/api/${n}`, repo: 'api', project: 'api' })
    const one = switcherGroups([entry('fix-a'), api('fix-b')], opts({ query: 'fix' }))
    expect(one.map((g) => g.label)).toEqual([null])
    const two = switcherGroups([entry('fix-a'), entry('fix-c'), api('fix-b'), api('fix-d')], opts({ query: 'fix' }))
    expect(two.map((g) => g.label).sort()).toEqual(['api', 'civet'])
    expect(two.every((g) => g.project)).toBe(true)
  })
})
