import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { GitAction, GitDiscardSnapshot, GitFileChange, GitStatusInfo } from '../types'

beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore, wsKey } = await import('./store')
const { useChanges } = await import('./changes')
const {
  changePaths,
  discardAllPaths,
  discardChanges,
  gitErrorMessage,
  runGitAction,
  stageChanges
} = await import('./git-actions')

const change = (path: string, patch: Partial<GitFileChange> = {}): GitFileChange => ({
  path,
  oldPath: null,
  kind: 'modified',
  additions: 1,
  deletions: 0,
  ...patch
})

const empty: GitStatusInfo = {
  staged: [],
  unstaged: [],
  untracked: [],
  conflicted: [],
  truncated: false
}

function withBackend(gitAction: CivetBackend['gitAction']) {
  const gitStatus = vi.fn(async () => empty)
  const gitBranch = vi.fn(async () => {
    throw new CivetError('unsupported', 'old')
  })
  useStore.setState({
    backend: { gitAction, gitStatus, gitBranch } as unknown as CivetBackend,
    toasts: []
  })
  return { gitStatus }
}

beforeEach(() => useChanges.setState({ byWorktree: {}, busy: {}, diffEpoch: {}, drafts: {} }))

describe('git actions', () => {
  it('condenses git errors to the lines that matter', () => {
    expect(
      gitErrorMessage(
        "git: To github.com:x/y.git\n ! [rejected]        feat -> feat (fetch first)\nerror: failed to push some refs to 'github.com:x/y.git'\nhint: Updates were rejected because the remote contains work that you do\nhint: not have locally."
      )
    ).toBe(
      "! [rejected]        feat -> feat (fetch first)\nfailed to push some refs to 'github.com:x/y.git'"
    )
    expect(gitErrorMessage('git: nothing to commit, working tree clean')).toBe(
      'nothing to commit, working tree clean'
    )
    expect(
      gitErrorMessage(
        'git: Auto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\nAutomatic merge failed; fix conflicts and then commit the result.'
      )
    ).toBe(
      'CONFLICT (content): Merge conflict in a.txt\nAutomatic merge failed; fix conflicts and then commit the result.'
    )
  })

  it('names both sides of a staged rename, and every non-conflict for "Discard all"', () => {
    const renamed = change('new.rs', { kind: 'renamed', oldPath: 'old.rs' })
    expect(changePaths('staged', [renamed])).toEqual(['new.rs', 'old.rs'])
    expect(changePaths('unstaged', [change('a')])).toEqual(['a'])
    expect(
      discardAllPaths({
        staged: [renamed, change('a')],
        unstaged: [change('a'), change('b')],
        untracked: [change('u', { kind: 'untracked' })]
      })
    ).toEqual(['new.rs', 'old.rs', 'a', 'b', 'u'])
  })

  it('marks the worktree busy, then refreshes and bumps the diff epoch', async () => {
    let release: () => void = () => {}
    const gitAction = vi.fn(
      () => new Promise<null>((r) => (release = () => r(null)))
    ) as unknown as CivetBackend['gitAction']
    const { gitStatus } = withBackend(gitAction)
    const key = wsKey('h', '/w')
    const p = stageChanges('h', '/w', 'unstaged', [change('a')])
    expect(useChanges.getState().busy[key]).toBe('Staging…')
    expect(gitAction).toHaveBeenCalledWith('h', '/w', { stage: { paths: ['a'] } })
    // A second action while one runs is refused, and the first stays busy.
    expect(await runGitAction('h', '/w', 'push', { busy: 'Pushing…', failed: 'Push' })).toBe(
      undefined
    )
    expect(gitAction).toHaveBeenCalledTimes(1)
    expect(useChanges.getState().busy[key]).toBe('Staging…')
    expect(useStore.getState().toasts.at(-1)?.title).toBe('Push: wait for "Staging…" to finish')
    release()
    await p
    expect(useChanges.getState().busy[key]).toBeUndefined()
    expect(useChanges.getState().diffEpoch[key]).toBe(1)
    expect(gitStatus).toHaveBeenCalled()
    // Unstaging a staged entry is the other request.
    const unstage = stageChanges('h', '/w', 'staged', [
      change('b', { kind: 'renamed', oldPath: 'c' })
    ])
    release()
    await unstage
    expect(gitAction).toHaveBeenLastCalledWith('h', '/w', { unstage: { paths: ['b', 'c'] } })
  })

  it('reports a failure as an error toast with git’s message', async () => {
    withBackend(async () => {
      throw new CivetError('git', 'git: hint: x\nfatal: no remote')
    })
    expect(
      await runGitAction('h', '/w', 'push', { busy: 'Pushing…', failed: 'Push failed' })
    ).toBeUndefined()
    expect(useStore.getState().toasts).toMatchObject([
      { kind: 'error', title: 'Push failed', body: 'no remote' }
    ])
    expect(useChanges.getState().busy[wsKey('h', '/w')]).toBeUndefined()
  })

  it('offers Undo after a discard, which sends the snapshot back', async () => {
    const snapshot: GitDiscardSnapshot = {
      entries: [
        { path: 'dir/a.txt', file: null, restoreIndex: false, index: null, untracked: false },
        {
          path: 'n.txt',
          file: { oid: 'abc1', mode: 0o100644 },
          restoreIndex: false,
          index: null,
          untracked: true
        }
      ]
    }
    const calls: GitAction[] = []
    withBackend(async (_h, _w, action) => {
      calls.push(action)
      return typeof action === 'object' && 'discard' in action ? snapshot : null
    })
    await discardChanges('h', '/w', ['dir/a.txt', 'n.txt'], false)
    expect(calls).toEqual([{ discard: { paths: ['dir/a.txt', 'n.txt'], staged: false } }])
    const toast = useStore.getState().toasts.at(-1)!
    expect(toast.title).toBe('Discarded changes to 2 files')
    expect(toast.action?.label).toBe('Undo')
    toast.action!.run()
    await vi.waitFor(() => expect(calls).toHaveLength(2))
    expect(calls[1]).toEqual({ restoreDiscarded: snapshot })
  })
})
