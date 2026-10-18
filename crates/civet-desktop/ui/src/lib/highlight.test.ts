import { describe, expect, it } from 'vitest'
import { MAX_HIGHLIGHT_CHARS, highlight, languageForFence, languageForPath } from './highlight'

describe('languageForPath', () => {
  it('maps extensions to Shiki ids', () => {
    expect(languageForPath('src/main.rs')).toBe('rust')
    expect(languageForPath('src/App.tsx')).toBe('tsx')
    expect(languageForPath('lib/a.TS')).toBe('typescript')
    expect(languageForPath('ci.yml')).toBe('yaml')
    expect(languageForPath('Cargo.toml')).toBe('toml')
    expect(languageForPath('scripts/x.sh')).toBe('shellscript')
  })

  it('knows some files by name', () => {
    expect(languageForPath('Dockerfile')).toBe('docker')
    expect(languageForPath('docker/Dockerfile.dev')).toBe('docker')
    expect(languageForPath('Makefile')).toBe('make')
    expect(languageForPath('Cargo.lock')).toBe('toml')
    expect(languageForPath('home/.zshrc')).toBe('shellscript')
  })

  it('returns null for unknown or missing extensions', () => {
    expect(languageForPath('LICENSE')).toBeNull()
    expect(languageForPath('notes.txt')).toBeNull()
    expect(languageForPath('.gitignore')).toBeNull()
    expect(languageForPath('a.toString')).toBeNull()
  })
})

describe('languageForFence', () => {
  it('reads the first word of the info string', () => {
    expect(languageForFence('ts')).toBe('typescript')
    expect(languageForFence('rust ignore')).toBe('rust')
    expect(languageForFence(' Bash ')).toBe('shellscript')
    expect(languageForFence('{.python}')).toBe('python')
    expect(languageForFence('json')).toBe('json')
  })

  it('returns null for unknown or empty', () => {
    expect(languageForFence('')).toBeNull()
    expect(languageForFence('text')).toBeNull()
    expect(languageForFence('constructor')).toBeNull()
  })
})

describe('highlight', () => {
  it('tokenizes with both themes as CSS variables', async () => {
    const lines = await highlight('const a = 1\nlet b', 'typescript')
    expect(lines).toHaveLength(2)
    expect(lines?.[0]?.map((t) => t.content).join('')).toBe('const a = 1')
    const keyword = lines?.[0]?.find((t) => t.content === 'const')
    expect(keyword?.style?.['--shiki-light']).toMatch(/^#/)
    expect(keyword?.style?.['--shiki-dark']).toMatch(/^#/)
  })

  it('skips unknown languages and long text', async () => {
    expect(await highlight('x', 'nope')).toBeNull()
    expect(await highlight('x'.repeat(MAX_HIGHLIGHT_CHARS + 1), 'typescript')).toBeNull()
  })
})
