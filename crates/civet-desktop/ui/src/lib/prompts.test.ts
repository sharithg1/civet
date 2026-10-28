import { describe, expect, it } from 'vitest'
import type { PaneInfo, WorktreeInfo } from '../types'
import { termSize } from '../types'
import {
  agentPanesOf,
  broadcastSummary,
  paneTargetLabel,
  promptPreview,
  targetGroups,
  triggerLabel
} from './prompts'

function pane(id: number, patch: Partial<PaneInfo> = {}): PaneInfo {
  return {
    id,
    worktree: '/w/a',
    cwd: '/w/a',
    command: ['claude'],
    title: 'claude',
    size: termSize(80, 24),
    agent: 'claude',
    status: 'running',
    exitCode: null,
    exited: false,
    createdAtMs: 0,
    state: 1,
    ...patch
  }
}

const wt = (id: string, branch: string | null): WorktreeInfo => ({
  id,
  repo: 'r',
  path: id,
  branch,
  head: null,
  isMain: false,
  locked: false
})

describe('prompt targets', () => {
  it('are the live agent panes of a worktree, oldest first', () => {
    const panes = {
      3: pane(3),
      1: pane(1),
      2: pane(2, { agent: null, command: ['zsh'] }),
      4: pane(4, { exited: true }),
      5: pane(5, { worktree: '/w/b' })
    }
    expect(agentPanesOf(panes, '/w/a').map((p) => p.id)).toEqual([1, 3])
    expect(agentPanesOf(undefined, '/w/a')).toEqual([])
  })

  it('group by worktree in the given order, leaving out worktrees without agents', () => {
    const panes = { h: { 1: pane(1), 2: pane(2, { worktree: '/w/c' }) } }
    const list = [
      { hostId: 'h', wt: wt('/w/c', null), project: 'civet', host: 'devbox' },
      { hostId: 'h', wt: wt('/w/b', 'empty'), project: 'civet', host: 'devbox' },
      { hostId: 'h', wt: wt('/w/a', 'feat/x'), project: 'civet', host: 'devbox' }
    ]
    const groups = targetGroups(list, panes, false)
    expect(groups.map((g) => [g.label, g.panes.map((p) => p.id)])).toEqual([
      ['civet · c', [2]],
      ['civet · feat/x', [1]]
    ])
    expect(targetGroups(list, panes, true)[0]!.label).toBe('civet · c · devbox')
  })

  it('label panes by agent and task', () => {
    expect(paneTargetLabel(pane(1))).toBe('Claude')
    expect(paneTargetLabel(pane(1, { title: 'Fix the flaky test' }))).toBe(
      'Claude · Fix the flaky test'
    )
    expect(paneTargetLabel(pane(1, { agent: 'codex', prompt: 'add docs' }))).toBe(
      'Codex · add docs'
    )
  })
})

describe('broadcast summary', () => {
  const ref = { hostId: 'h', pane: 1 }
  it('counts agents', () => {
    expect(broadcastSummary('now', { ok: 3, failed: [] })).toEqual({
      kind: 'info',
      title: 'Sent to 3 agents'
    })
    expect(broadcastSummary('done', { ok: 1, failed: [] }).title).toBe('Queued for 1 agent')
  })

  it('reports failures with the first message', () => {
    expect(
      broadcastSummary('now', { ok: 1, failed: [{ ref, message: 'pane 1 has exited' }] })
    ).toEqual({
      kind: 'error',
      title: 'Sent to 1 of 2 agents',
      body: 'pane 1 has exited'
    })
    const failed = [
      { ref, message: 'unsupported' },
      { ref, message: 'x' }
    ]
    expect(broadcastSummary('done', { ok: 0, failed })).toEqual({
      kind: 'error',
      title: 'Could not queue for 2 agents',
      body: 'unsupported (and 1 more)'
    })
  })
})

describe('prompt text', () => {
  it('previews the first line', () => {
    expect(promptPreview('  run   the tests  ')).toBe('run the tests')
    expect(promptPreview('\nfirst\nsecond')).toBe('first …')
    expect(promptPreview('x'.repeat(100), 10)).toBe(`${'x'.repeat(9)}…`)
  })

  it('names triggers', () => {
    expect(triggerLabel('done')).toBe('when done')
    expect(triggerLabel('doneOrNeedsInput')).toBe('when done or needs input')
  })
})
