import { describe, expect, it } from 'vitest'
import { agentStepKind, describeTemplate, draftOf, EMPTY_TEMPLATE, templateOf } from './templates'

describe('worktree templates', () => {
  it('turns editor fields into a host template: blank is unset', () => {
    expect(templateOf({ setup: '  \n', copy: '', agent: ' ', prompt: '' })).toEqual(EMPTY_TEMPLATE)
    expect(
      templateOf({
        setup: 'pnpm install\ncp a b\n\n',
        copy: '.env\n apps/*/.env , .env.local\n',
        agent: ' claude --model opus ',
        prompt: ' Read CLAUDE.md. '
      })
    ).toEqual({
      setup: 'pnpm install\ncp a b',
      copy: ['.env', 'apps/*/.env', '.env.local'],
      agent: 'claude --model opus',
      prompt: 'Read CLAUDE.md.'
    })
    const t = { setup: 'make', copy: ['.env', 'x'], agent: 'codex', prompt: null }
    expect(templateOf(draftOf(t))).toEqual(t)
  })

  it('summarizes what a template does', () => {
    expect(describeTemplate(EMPTY_TEMPLATE)).toBe('')
    expect(
      describeTemplate({
        setup: '#!/usr/bin/env bash\n# deps\npnpm install\npnpm build\n',
        copy: ['.env'],
        agent: null,
        prompt: null
      })
    ).toBe('pnpm install (+1) · copies .env')
    expect(describeTemplate({ ...EMPTY_TEMPLATE, setup: '# only a comment' })).toBe('')
  })

  it('picks the step kind from the agent command', () => {
    expect(agentStepKind('codex --full-auto')).toBe('codex')
    expect(agentStepKind('/opt/bin/codex')).toBe('codex')
    expect(agentStepKind('claude --model opus')).toBe('agent')
    expect(agentStepKind('npx @openai/codex')).toBe('agent')
  })
})
