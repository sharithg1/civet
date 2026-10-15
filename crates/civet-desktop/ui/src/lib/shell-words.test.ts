import { describe, expect, it } from 'vitest'
import { splitShellWords } from './shell-words'

describe('splitShellWords', () => {
  it('splits on whitespace and honours quotes and escapes', () => {
    expect(splitShellWords('aider --model sonnet')).toEqual(['aider', '--model', 'sonnet'])
    expect(splitShellWords(`claude -p 'fix the bug' --x "a \\"b\\" c"`)).toEqual([
      'claude',
      '-p',
      'fix the bug',
      '--x',
      'a "b" c'
    ])
    expect(splitShellWords('a\\ b  ""')).toEqual(['a b', ''])
    expect(splitShellWords('   ')).toEqual([])
  })
  it('rejects unterminated quotes', () => {
    expect(() => splitShellWords("a 'b")).toThrow()
    expect(() => splitShellWords('a "b')).toThrow()
  })
})
