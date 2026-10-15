import { describe, expect, it } from 'vitest'
import type { PaneInfo, WorktreeInfo } from '../types'
import {
  ago,
  cleanTitle,
  duration,
  fuzzy,
  highlightRuns,
  matchFields,
  paneDot,
  paneWhat,
  promptLabel,
  shortPath,
  splitBranch,
  tabLabels,
  worktreeDot,
  worktreeTask
} from './worktree-view'

const pane = (p: Partial<PaneInfo>): PaneInfo => ({
  id: 1,
  worktree: '/wt',
  cwd: '/wt',
  command: ['bash'],
  title: '',
  size: { cols: 80, rows: 24, pixelWidth: 0, pixelHeight: 0 },
  agent: null,
  status: 'unknown',
  exitCode: null,
  exited: false,
  createdAtMs: 0,
  state: 1,
  ...p
})
const wt: WorktreeInfo = {
  id: '/wt',
  repo: 'r',
  path: '/wt',
  branch: 'NO-TICKET/fix-pdf',
  head: null,
  isMain: false,
  locked: false
}

describe('status dots', () => {
  it('maps panes', () => {
    expect(paneDot(pane({ agent: 'claude', status: 'running' }), true)).toBe('run')
    expect(paneDot(pane({ agent: 'claude', status: 'needsInput' }), true)).toBe('wait')
    expect(paneDot(pane({ agent: 'claude', exited: true, exitCode: 2 }), true)).toBe('err')
    expect(paneDot(pane({ agent: 'claude', status: 'done' }), true)).toBe('idle')
    expect(paneDot(pane({}), true)).toBe('idle')
    expect(paneDot(pane({ agent: 'claude', status: 'running' }), false)).toBe('disc')
  })
  it('aggregates with attention priority and unread done', () => {
    const panes = [pane({ agent: 'c', status: 'running' }), pane({ id: 2, agent: 'c', status: 'needsInput' })]
    expect(worktreeDot(panes, true)).toBe('wait')
    expect(worktreeDot([pane({ agent: 'c', status: 'done' })], true, 'done')).toBe('ok')
    expect(worktreeDot([], true)).toBe('idle')
    expect(worktreeDot(panes, false)).toBe('disc')
  })
})

describe('labels', () => {
  it('splits branches and cleans titles', () => {
    expect(splitBranch('NO-TICKET/fix-pdf')).toEqual({ prefix: 'NO-TICKET/', name: 'fix-pdf' })
    expect(splitBranch('main')).toEqual({ prefix: '', name: 'main' })
    expect(cleanTitle('✳ Worktree cleanup')).toBe('Worktree cleanup')
  })
  it('derives the task from the oldest agent pane title', () => {
    expect(worktreeTask(wt, [pane({ agent: 'claude', title: 'fix-pdf' })])).toBeNull()
    expect(
      worktreeTask(wt, [pane({ id: 3, agent: 'claude', title: '⠂ Fix blank pages' }), pane({ title: 'zsh' })])
    ).toBe('Fix blank pages')
  })
  it('numbers tabs by kind', () => {
    const c = pane({ agent: 'claude' })
    expect(tabLabels([[c], [undefined], [c], [pane({})]]).map((t) => t.label)).toEqual([
      'Claude 1',
      'Terminal',
      'Claude 2',
      'Terminal 2'
    ])
  })
  it('formats paths and times', () => {
    expect(shortPath('/home/ubuntu/wt/x')).toBe('~/wt/x')
    expect(ago(0, 30 * 60000)).toBe('30m')
    expect(ago(0, 3 * 3600000)).toBe('3h')
    expect(duration(0, (3 * 60 + 12) * 60000)).toBe('3h 12m')
  })
})

describe('fuzzy', () => {
  it('matches subsequences and prefers word starts', () => {
    expect(fuzzy('pdf', 'fix blank pages in the pdf')).not.toBeNull()
    expect(fuzzy('xyz', 'abc')).toBeNull()
    const start = fuzzy('fb', 'fix-blank')!
    const mid = fuzzy('fb', 'xfxb')!
    expect(start.score).toBeGreaterThan(mid.score)
  })
  it('requires every token to hit some field', () => {
    const m = matchFields(['pdf', 'nucleus'], ['Fix PDF', 'fix-pdf', 'nucleus-api'])!
    expect(m.hits[2]).toEqual([0, 1, 2, 3, 4, 5, 6])
    expect(matchFields(['zzz'], ['a', 'b'])).toBeNull()
  })
  it('splits highlight runs', () => {
    expect(highlightRuns('abc', [1])).toEqual([
      { s: 'a', hit: false },
      { s: 'b', hit: true },
      { s: 'c', hit: false }
    ])
  })
})

describe('session label from the first prompt', () => {
  const claude = (p: Partial<PaneInfo>) => pane({ agent: 'claude', command: ['claude'], ...p })

  it('promptLabel: first line, collapsed, capped; no slash commands', () => {
    expect(promptLabel('  fix the   copay\ndisplay\n')).toBe('fix the copay')
    expect(promptLabel('\n\nhi')).toBe('hi')
    expect(promptLabel('/clear')).toBeNull()
    expect(promptLabel(null)).toBeNull()
    expect(promptLabel('x'.repeat(100))).toHaveLength(80)
  })

  it('placeholder title falls back to the prompt, a real title wins', () => {
    expect(paneWhat(claude({ title: '✳ Claude Code' })).text).toBe('new session')
    expect(paneWhat(claude({ title: '✳ Claude Code', prompt: 'hi' })).text).toBe('hi')
    expect(paneWhat(claude({ title: '✳ Worktree cleanup', prompt: 'hi' })).text).toBe('Worktree cleanup')
    expect(worktreeTask(wt, [claude({ title: '✳ Claude Code', prompt: 'Fix copay' })])).toBe('Fix copay')
    expect(worktreeTask(wt, [claude({ title: '✳ Claude Code' })])).toBeNull()
  })
})
