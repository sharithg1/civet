import { beforeAll, beforeEach, describe, expect, it } from 'vitest'
import { CivetError, type CivetBackend } from '../backend'
import type { DaemonEventEnvelope, PaneInfo, QueuedPrompt } from '../types'
import { termSize } from '../types'

// The store touches window/document only in initApp (listeners); give node a stub.
beforeAll(() => {
  const target = { addEventListener: () => {}, removeEventListener: () => {} }
  Object.assign(globalThis, {
    window: { ...target, innerWidth: 1400 },
    document: { ...target, hasFocus: () => true }
  })
})

const { useStore } = await import('./store')
type HostView = import('./store').HostView
const prompts = await import('./prompts')
const { usePrompts } = prompts

function pane(id: number, patch: Partial<PaneInfo> = {}): PaneInfo {
  return {
    id,
    worktree: '/w/a',
    cwd: '/w/a',
    command: ['claude'],
    title: `agent ${id}`,
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

const queued = (id: number, pane: number, text = `p${id}`): QueuedPrompt => ({
  id,
  pane,
  text,
  trigger: 'done',
  createdAtMs: 0
})

type Calls = { send: [string, number, string][]; queue: [string, number, string, string][] }

/** A backend with just the prompt calls; pane 9 fails like an exited pane. */
async function init(): Promise<Calls> {
  const calls: Calls = { send: [], queue: [] }
  let next = 100
  let listener: ((e: DaemonEventEnvelope) => void) | null = null
  const b = {
    kind: 'mock',
    sendPrompt: async (hostId: string, p: number, text: string) => {
      if (p === 9) throw new CivetError('invalidArgument', 'pane 9 has exited')
      calls.send.push([hostId, p, text])
    },
    queuePrompt: async (hostId: string, p: number, text: string, trigger: string) => {
      calls.queue.push([hostId, p, text, trigger])
      return { ...queued(next++, p, text), trigger }
    },
    listQueuedPrompts: async () => [queued(1, 1), queued(2, 1), queued(3, 2)],
    onDaemonEvent: async (cb: (e: DaemonEventEnvelope) => void) => {
      listener = cb
      return () => {
        listener = null
      }
    }
  } as unknown as CivetBackend
  void listener
  useStore.setState({ backend: b, toasts: [], hosts: {}, panes: {} })
  usePrompts.setState({ queues: {}, selected: [], dialog: null })
  await start(b)
  return calls
}

/** initPrompts, after stopping the previous test's (its host sync would list too). */
let stop: (() => void) | null = null
async function start(b: CivetBackend): Promise<void> {
  stop?.()
  stop = await prompts.initPrompts(b)
}

beforeEach(() => {
  useStore.setState({ toasts: [] })
})

describe('queued prompts', () => {
  it('load when a host connects and follow promptQueue events', async () => {
    await init()
    useStore.setState({
      hosts: {
        h: {
          hostId: 'h',
          label: 'h',
          state: 'connected',
          transport: 'quic',
          daemonVersion: '0.4.4',
          instanceId: '1',
          upgrade: null,
          rttMs: null,
          message: null,
          bootstrap: null,
          errorCode: null
        }
      },
      panes: { h: { 1: pane(1), 2: pane(2) } }
    })
    await new Promise((r) => setTimeout(r, 0))
    expect(usePrompts.getState().queues.h).toEqual({
      1: [queued(1, 1), queued(2, 1)],
      2: [queued(3, 2)]
    })

    // Sent by the daemon: the queue shrinks and a toast names the prompt.
    prompts.onPromptEvent('h', { promptQueue: { pane: 1, prompts: [queued(2, 1)], sent: 1 } })
    expect(usePrompts.getState().queues.h![1]).toEqual([queued(2, 1)])
    expect(useStore.getState().toasts.at(-1)).toMatchObject({
      kind: 'info',
      title: 'Queued prompt sent to agent 1',
      body: 'p1',
      target: { hostId: 'h', worktree: '/w/a', pane: 1 }
    })
    // Emptied (cancelled elsewhere) and closed panes drop out.
    prompts.onPromptEvent('h', { promptQueue: { pane: 1, prompts: [], sent: null } })
    prompts.onPromptEvent('h', { paneClosed: { pane: 2 } })
    expect(usePrompts.getState().queues.h).toEqual({})
    expect(useStore.getState().toasts).toHaveLength(1)
  })

  it('a list that a queue event overtook is fetched again', async () => {
    let lists = 0
    const b = {
      kind: 'mock',
      // The first list was taken before pane 1's queue changed.
      listQueuedPrompts: async () => {
        lists++
        if (lists === 1) {
          prompts.onPromptEvent('h', {
            promptQueue: { pane: 1, prompts: [queued(5, 1, 'new')], sent: null }
          })
          return [queued(1, 1, 'old')]
        }
        return [queued(5, 1, 'new'), queued(6, 2)]
      },
      onDaemonEvent: async () => () => {}
    } as unknown as CivetBackend
    useStore.setState({ backend: b, toasts: [], hosts: {}, panes: {} })
    usePrompts.setState({ queues: {}, selected: [], dialog: null })
    await start(b)
    useStore.setState({
      hosts: {
        h: { hostId: 'h', state: 'connected', daemonVersion: '0.4.4' } as HostView
      }
    })
    await new Promise((r) => setTimeout(r, 0))
    expect(lists).toBe(2)
    expect(usePrompts.getState().queues.h).toEqual({
      1: [queued(5, 1, 'new')],
      2: [queued(6, 2)]
    })
  })

  it('an old daemon is not asked', () => {
    const host = (daemonVersion: string | null, state: 'connected' | 'reconnecting') =>
      ({ state, daemonVersion }) as Parameters<typeof prompts.hasPrompts>[0]
    expect(prompts.hasPrompts(host('0.4.4', 'connected'))).toBe(true)
    expect(prompts.hasPrompts(host(null, 'connected'))).toBe(true)
    expect(prompts.hasPrompts(host('0.4.3', 'connected'))).toBe(false)
    expect(prompts.hasPrompts(host('0.5.0', 'reconnecting'))).toBe(false)
  })
})

describe('broadcast', () => {
  it('sends to every target once and sums up failures', async () => {
    const calls = await init()
    const out = await prompts.broadcastPrompt(
      [
        { hostId: 'h', pane: 1 },
        { hostId: 'g', pane: 1 },
        { hostId: 'h', pane: 1 },
        { hostId: 'h', pane: 9 }
      ],
      'run the tests',
      'now'
    )
    expect(calls.send).toEqual([
      ['h', 1, 'run the tests'],
      ['g', 1, 'run the tests']
    ])
    expect(out.ok).toBe(2)
    expect(out.failed).toEqual([{ ref: { hostId: 'h', pane: 9 }, message: 'pane 9 has exited' }])
    expect(useStore.getState().toasts.at(-1)).toMatchObject({
      kind: 'error',
      title: 'Sent to 2 of 3 agents',
      body: 'pane 9 has exited'
    })
  })

  it('queues on each target and shows the queues at once', async () => {
    const calls = await init()
    await prompts.broadcastPrompt(
      [
        { hostId: 'h', pane: 1 },
        { hostId: 'h', pane: 2 }
      ],
      'then commit',
      'doneOrNeedsInput'
    )
    expect(calls.queue.map((c) => c[3])).toEqual(['doneOrNeedsInput', 'doneOrNeedsInput'])
    const q = usePrompts.getState().queues.h!
    expect(q[1]!.map((p) => p.text)).toEqual(['then commit'])
    expect(q[2]!.map((p) => p.text)).toEqual(['then commit'])
    expect(useStore.getState().toasts.at(-1)).toMatchObject({ title: 'Queued for 2 agents' })
  })

  it('targets the sidebar selection, else the given or active worktree', async () => {
    await init()
    useStore.setState({
      active: { hostId: 'h', worktree: '/w/a' },
      panes: {
        h: {
          1: pane(1),
          2: pane(2, { worktree: '/w/b' }),
          3: pane(3, { agent: null }),
          4: pane(4, { worktree: '/w/b', exited: true })
        }
      }
    })
    const targets = () => {
      const d = usePrompts.getState().dialog
      return d?.kind === 'send' ? d.targets : null
    }
    prompts.openSendPrompt()
    expect(targets()).toEqual([{ hostId: 'h', pane: 1 }])
    prompts.openSendPrompt(['h\u0000/w/b'])
    expect(targets()).toEqual([{ hostId: 'h', pane: 2 }])
    prompts.toggleSelected('h\u0000/w/a')
    prompts.toggleSelected('h\u0000/w/b')
    prompts.openSendPrompt(['h\u0000/w/b'])
    expect(targets()).toEqual([
      { hostId: 'h', pane: 1 },
      { hostId: 'h', pane: 2 }
    ])
    prompts.toggleSelected('h\u0000/w/a')
    expect(usePrompts.getState().selected).toEqual(['h\u0000/w/b'])
    prompts.clearSelected()
    expect(usePrompts.getState().selected).toEqual([])
  })
})
