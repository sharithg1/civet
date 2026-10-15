import { describe, expect, it } from 'vitest'
import { aggregateStatus, indicatorFor } from './agent-status'

describe('agent status', () => {
  it('aggregates by attention priority', () => {
    expect(aggregateStatus([])).toBe('unknown')
    expect(aggregateStatus([{ status: 'done' }, { status: 'running' }])).toBe('running')
    expect(aggregateStatus([{ status: 'running' }, { status: 'needsInput' }])).toBe('needsInput')
    expect(aggregateStatus([{ status: 'idle' }, { status: 'done' }])).toBe('done')
  })
  it('maps to the Orca indicator vocabulary', () => {
    expect(indicatorFor('running')).toBe('working')
    expect(indicatorFor('needsInput')).toBe('permission')
    expect(indicatorFor('done')).toBe('done')
    expect(indicatorFor('unknown')).toBe('inactive')
  })
})
