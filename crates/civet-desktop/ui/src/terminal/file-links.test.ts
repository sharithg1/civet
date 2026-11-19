import type { IBufferLine, ILink, Terminal } from '@xterm/xterm'
import { describe, expect, it, vi } from 'vitest'
import type { HostFile } from '../types'
import { FileResolver, type FileLink, fileLinkProvider, lineCells } from './file-links'

/** A buffer line of `text`; characters in `wide` take two cells. */
function bufferLine(text: string, wide = ''): IBufferLine {
  const cells: { chars: string; width: number }[] = []
  for (const ch of text) {
    if (wide.includes(ch)) cells.push({ chars: ch, width: 2 }, { chars: '', width: 0 })
    else cells.push({ chars: ch === ' ' ? '' : ch, width: 1 })
  }
  return {
    length: cells.length,
    getCell: (x: number) => {
      const c = cells[x]
      return c && { getChars: () => c.chars, getWidth: () => c.width }
    }
  } as unknown as IBufferLine
}

function termWith(lines: IBufferLine[]): Terminal {
  return { buffer: { active: { getLine: (y: number) => lines[y] } } } as unknown as Terminal
}

const provide = (
  term: Terminal,
  resolver: FileResolver,
  y: number,
  onOpen: (event: MouseEvent, link: FileLink) => void = () => {}
) =>
  new Promise<ILink[] | undefined>((resolve) =>
    fileLinkProvider(term, resolver, onOpen).provideLinks(y, resolve)
  )

const statIn =
  (files: Record<string, HostFile>) =>
  async (path: string): Promise<HostFile> => {
    const f = files[path]
    if (!f) throw new Error('not found')
    return f
  }

describe('lineCells', () => {
  it('maps text to cells past wide characters and blanks', () => {
    const { text, cols } = lineCells(bufferLine('好 a.md', '好'))
    expect(text).toBe('好 a.md')
    expect(cols).toEqual([0, 2, 3, 4, 5, 6])
  })
})

describe('fileLinkProvider', () => {
  const shot = { path: '/tmp/shot.png', size: 10 }
  const app = { path: '/w/src/app.ts', size: 20 }

  it('links only the paths the host has, at their cells', async () => {
    const term = termWith([bufferLine('好 saved /tmp/shot.png and/or src/app.ts:12', '好')])
    const resolver = new FileResolver(statIn({ '/tmp/shot.png': shot, 'src/app.ts': app }))
    const opened: FileLink[] = []
    const links = await provide(term, resolver, 1, (_e, link) => opened.push(link))
    expect(links?.map((l) => l.text)).toEqual(['/tmp/shot.png', 'src/app.ts:12'])
    // 1-based, inclusive, shifted one cell by the wide character.
    expect(links![0]!.range).toEqual({ start: { x: 10, y: 1 }, end: { x: 22, y: 1 } })
    links![1]!.activate({} as MouseEvent, links![1]!.text)
    expect(opened).toEqual([{ file: app, line: 12 }])
  })

  it('has no links for a line without files, or one that is gone', async () => {
    const term = termWith([bufferLine('nothing here'), bufferLine('see missing/file.txt')])
    const resolver = new FileResolver(statIn({}))
    expect(await provide(term, resolver, 1)).toBeUndefined()
    expect(await provide(term, resolver, 2)).toBeUndefined()
    expect(await provide(term, resolver, 9)).toBeUndefined()
  })
})

describe('FileResolver', () => {
  it('asks once per path until the answer is stale, misses included', async () => {
    let now = 0
    const stat = vi.fn(async (path: string) => {
      if (path === 'gone') throw new Error('not found')
      return { path: `/w/${path}`, size: 1 }
    })
    const r = new FileResolver(stat, 1000, () => now)
    expect(await r.resolve('a')).toEqual({ path: '/w/a', size: 1 })
    expect(await r.resolve('gone')).toBeNull()
    await r.resolve('a')
    await r.resolve('gone')
    expect(stat).toHaveBeenCalledTimes(2)
    now = 1000
    await r.resolve('a')
    expect(stat).toHaveBeenCalledTimes(3)
  })

  it('is null when asking throws', async () => {
    const r = new FileResolver(() => {
      throw new Error('not connected')
    })
    expect(await r.resolve('a')).toBeNull()
  })
})
