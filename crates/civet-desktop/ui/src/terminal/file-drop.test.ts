import { describe, expect, it } from 'vitest'
import {
  clipboardImage,
  clipboardImageName,
  dropChunks,
  isImagePath,
  shellEscapePath
} from './file-drop'

describe('file drop', () => {
  it('recognises image files by the basename extension', () => {
    expect(isImagePath('/a/Shot.PNG')).toBe(true)
    expect(isImagePath('/home/jane.doe/photo')).toBe(false)
    expect(isImagePath('/a/notes.txt')).toBe(false)
  })

  it('pastes a safe image path raw', () => {
    expect(dropChunks(['/tmp/a.png'])).toEqual([{ kind: 'paste', text: '/tmp/a.png' }])
  })

  it('backslash-escapes an image path with spaces', () => {
    expect(dropChunks(['/Users/me/Screen Shot (1).png'])).toEqual([
      { kind: 'paste', text: '/Users/me/Screen\\ Shot\\ \\(1\\).png' }
    ])
  })

  it('types other files shell-escaped with a trailing space', () => {
    expect(dropChunks(['/a/b.ts', "/a/it's.md"])).toEqual([
      { kind: 'type', text: '/a/b.ts ' },
      { kind: 'type', text: "'/a/it'\\''s.md' " }
    ])
    expect(shellEscapePath('/a b')).toBe("'/a b'")
  })

  it('separates an image paste from following input', () => {
    expect(dropChunks(['/a.png', '/b.png'])).toEqual([
      { kind: 'paste', text: '/a.png' },
      { kind: 'paste', text: '/b.png' }
    ])
    expect(dropChunks(['/a.png', '/b.ts'])).toEqual([
      { kind: 'paste', text: '/a.png' },
      { kind: 'type', text: ' ' },
      { kind: 'type', text: '/b.ts ' }
    ])
  })

  it('takes the image from a paste only when there is no text', () => {
    const file = { name: 'image.png' } as File
    const data = (text: string) => ({
      getData: () => text,
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }]
    })
    expect(clipboardImage(data(''))).toBe(file)
    expect(clipboardImage(data('hello'))).toBeNull()
    expect(clipboardImage({ getData: () => '', items: [] })).toBeNull()
    expect(clipboardImage(null)).toBeNull()
  })

  it('names pasted images by type and time', () => {
    const at = new Date('2026-10-15T11:02:03.456Z')
    expect(clipboardImageName('image/png', at)).toBe('clipboard-20261015-110203.png')
    expect(clipboardImageName('image/jpeg', at)).toBe('clipboard-20261015-110203.jpg')
  })
})
