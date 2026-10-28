import { describe, expect, it } from 'vitest'
import type { AgentTurn } from '../types'
import {
  dayLabel,
  isTurn,
  spanLength,
  turnLength,
  turnLines,
  turnsByDay,
  turnTimes
} from './activity'

const turn = (
  id: number,
  startedMs: number,
  endedMs: number | null,
  extra: Partial<AgentTurn> = {}
): AgentTurn => ({
  id,
  pane: 1,
  agent: 'claude',
  prompt: null,
  startedMs,
  endedMs,
  status: endedMs == null ? 'running' : 'done',
  message: null,
  files: [],
  filesTruncated: false,
  fromTree: null,
  toTree: null,
  ...extra
})

/** Local time on 2026-10-27. */
const at = (h: number, m = 0, day = 27) => new Date(2026, 9, day, h, m).getTime()

describe('activity view helpers', () => {
  it('matches a diff tab to its turn by id and snapshots', () => {
    const t = turn(3, at(9), at(10), { fromTree: 'a', toTree: 'b' })
    expect(isTurn({ id: 3, from: 'a', to: 'b' }, t)).toBe(true)
    // The same id in a timeline started over.
    expect(isTurn({ id: 3, from: 'c', to: 'd' }, t)).toBe(false)
    expect(isTurn({ id: 4, from: 'a', to: 'b' }, t)).toBe(false)
  })

  it('formats span lengths', () => {
    expect(spanLength(0)).toBe('0s')
    expect(spanLength(44_600)).toBe('45s')
    expect(spanLength(192_000)).toBe('3m 12s')
    expect(spanLength(3_840_000)).toBe('1h 4m')
    expect(spanLength(-5)).toBe('0s')
    // A running turn counts up to now.
    expect(turnLength(turn(1, at(9), null), at(9, 2))).toBe('2m 0s')
    expect(turnLength(turn(1, at(9), at(9, 1)), at(12))).toBe('1m 0s')
  })

  it('labels days and times', () => {
    const now = at(15)
    expect(dayLabel(at(0, 1), now)).toBe('Today')
    expect(dayLabel(at(23, 59, 26), now)).toBe('Yesterday')
    expect(dayLabel(at(12, 0, 20), now)).not.toMatch(/Today|Yesterday/)
    expect(turnTimes(turn(1, at(9), null))).toMatch(/–now$/)
    expect(turnTimes(turn(1, at(9), at(10)))).not.toContain('now')
  })

  it('groups turns newest first by day', () => {
    const now = at(15)
    const groups = turnsByDay(
      [
        turn(1, at(22, 0, 25), at(23, 0, 25)),
        turn(2, at(9), at(10)),
        turn(3, at(14), null),
        turn(4, at(8, 0, 26), at(9, 0, 26))
      ],
      now
    )
    expect(groups.map((g) => [g.day.length > 0, g.turns.map((t) => t.id)])).toEqual([
      [true, [3, 2]],
      [true, [4]],
      [true, [1]]
    ])
    expect(groups.slice(0, 2).map((g) => g.day)).toEqual(['Today', 'Yesterday'])
    expect(turnsByDay([], now)).toEqual([])
  })

  it('sums line counts, binary files as none', () => {
    const t = turn(1, 0, 1, {
      files: [
        { path: 'a', oldPath: null, kind: 'modified', additions: 3, deletions: 1 },
        { path: 'b.png', oldPath: null, kind: 'modified', additions: null, deletions: null },
        { path: 'c', oldPath: null, kind: 'added', additions: 10, deletions: 0 }
      ]
    })
    expect(turnLines(t)).toEqual({ additions: 13, deletions: 1 })
  })
})
