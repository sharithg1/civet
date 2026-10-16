import { describe, expect, it } from 'vitest'
import { completeFolder, folderQuery, matchFolders } from './folder-complete'

const dirs = [
  { name: '.config', gitRepo: false },
  { name: 'Civet', gitRepo: false },
  { name: 'civet', gitRepo: true },
  { name: 'civet-worktrees', gitRepo: false },
  { name: 'orca', gitRepo: true }
]

describe('folderQuery', () => {
  it('lists the directory before the last slash', () => {
    expect(folderQuery('~/src/ci')).toEqual({ dir: '~/src/', prefix: 'ci' })
    expect(folderQuery('~/src/')).toEqual({ dir: '~/src/', prefix: '' })
    expect(folderQuery('/')).toEqual({ dir: '/', prefix: '' })
    expect(folderQuery('/ho')).toEqual({ dir: '/', prefix: 'ho' })
    expect(folderQuery('~/')).toEqual({ dir: '~/', prefix: '' })
    expect(folderQuery('~')).toEqual({ dir: '~', prefix: '' })
  })

  it('skips empty and relative paths', () => {
    expect(folderQuery('')).toBeNull()
    expect(folderQuery('src/civet')).toBeNull()
    expect(folderQuery('~user/x')).toBeNull()
  })
})

describe('matchFolders', () => {
  it('matches by prefix ignoring case, exact case first, hidden only for a dot', () => {
    expect(matchFolders(dirs, 'ci').map((d) => d.name)).toEqual([
      'civet',
      'civet-worktrees',
      'Civet'
    ])
    expect(matchFolders(dirs, '').map((d) => d.name)).toEqual([
      'Civet',
      'civet',
      'civet-worktrees',
      'orca'
    ])
    expect(matchFolders(dirs, '.').map((d) => d.name)).toEqual(['.config'])
    expect(matchFolders(dirs, 'x')).toEqual([])
  })
})

describe('completeFolder', () => {
  it('appends the name and a slash so its subfolders list next', () => {
    expect(completeFolder({ dir: '~/src/', prefix: 'ci' }, 'civet')).toBe('~/src/civet/')
    expect(completeFolder({ dir: '~', prefix: '' }, 'src')).toBe('~/src/')
    expect(completeFolder({ dir: '/', prefix: 'h' }, 'home')).toBe('/home/')
  })
})
