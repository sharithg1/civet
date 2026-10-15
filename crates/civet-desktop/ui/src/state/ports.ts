// Listening ports and port forwards per host (right panel → Ports).
//
// Ports come from the daemon (`civet://ports`), forwards from the core
// (`civet://forwards`); both are host-wide. Pinned forwards are kept in
// prefs and re-created whenever their host connects (the core drops a
// host's forwards when it disconnects; reconnects and daemon restarts keep
// them).

import { create } from 'zustand'
import { CivetError, type CivetBackend } from '@/backend'
import type { ForwardView, PortsEvent } from '@/types'
import { setPrefs, usePrefs, type PinnedForward } from './prefs'
import { pushToast, reportError, useStore } from './store'

type PortsState = {
  /** hostId → last ports report */
  ports: Record<string, PortsEvent>
  /** hostId → forwards, by remote port */
  forwards: Record<string, ForwardView[]>
}

export const usePorts = create<PortsState>(() => ({ ports: {}, forwards: {} }))

let backend: CivetBackend | null = null

function b(): CivetBackend {
  if (!backend) throw new Error('ports not initialised')
  return backend
}

function setPorts(e: PortsEvent): void {
  usePorts.setState((s) => ({ ports: { ...s.ports, [e.hostId]: e } }))
}

function setForwards(hostId: string, forwards: ForwardView[]): void {
  const sorted = [...forwards].sort((x, y) => x.remotePort - y.remotePort)
  usePorts.setState((s) => ({ forwards: { ...s.forwards, [hostId]: sorted } }))
}

/** Listen for ports / forwards and restore pinned forwards on connect. */
export async function initPorts(be: CivetBackend): Promise<() => void> {
  backend = be
  const unlisten = await Promise.all([
    be.onPorts(setPorts),
    be.onForwards((e) => setForwards(e.hostId, e.forwards))
  ])
  const connected = new Set<string>()
  const sync = (hosts: ReturnType<typeof useStore.getState>['hosts']) => {
    for (const [id, h] of Object.entries(hosts)) {
      if (h.state === 'connected' && !connected.has(id)) {
        connected.add(id)
        void loadHost(id)
      } else if (h.state === 'disconnected' || h.state === 'failed') {
        connected.delete(id)
      }
    }
    for (const id of [...connected]) if (!hosts[id]) connected.delete(id)
  }
  sync(useStore.getState().hosts)
  const unsub = useStore.subscribe((s, prev) => {
    if (s.hosts !== prev.hosts) sync(s.hosts)
  })
  return () => {
    unsub()
    unlisten.forEach((u) => u())
  }
}

/** Fetch a host's state (after a UI reload or a connect) and restore its pins. */
async function loadHost(hostId: string): Promise<void> {
  try {
    const [ports, forwards] = await Promise.all([b().listPorts(hostId), b().listForwards(hostId)])
    setPorts(ports)
    setForwards(hostId, forwards)
    await restorePins(hostId, forwards)
  } catch (err) {
    console.warn('[civet] ports state', hostId, err)
  }
}

async function restorePins(hostId: string, existing: ForwardView[]): Promise<void> {
  const pins = usePrefs.getState().pinnedForwards[hostId] ?? []
  for (const pin of pins) {
    if (existing.some((f) => f.remotePort === pin.remote)) continue
    try {
      await b().forwardPort(hostId, pin.remote, pin.local)
    } catch (err) {
      // Its local port is taken now: forward anyway on another one.
      if (CivetError.from(err).code !== 'alreadyExists') {
        reportError(`Could not restore the forward of port ${pin.remote}`, err)
        continue
      }
      try {
        const f = await b().forwardPort(hostId, pin.remote, null)
        pushToast(
          {
            kind: 'info',
            title: `localhost:${pin.local} is taken`,
            body: `Port ${pin.remote} is forwarded to localhost:${f.localPort} instead.`
          },
          6000
        )
      } catch (err2) {
        reportError(`Could not restore the forward of port ${pin.remote}`, err2)
      }
    }
  }
}

function setPins(hostId: string, f: (pins: PinnedForward[]) => PinnedForward[]): void {
  const all = usePrefs.getState().pinnedForwards
  const next = f(all[hostId] ?? [])
  const rest = Object.fromEntries(Object.entries(all).filter(([k]) => k !== hostId))
  setPrefs({ pinnedForwards: next.length ? { ...rest, [hostId]: next } : rest })
}

export function isPinned(pins: Record<string, PinnedForward[]>, hostId: string, remote: number): boolean {
  return (pins[hostId] ?? []).some((p) => p.remote === remote)
}

/** Forward `remote`; `local` null = same number if free. Toasts a remap. */
export async function forwardPort(
  hostId: string,
  remote: number,
  local: number | null = null,
  opts: { pin?: boolean } = {}
): Promise<ForwardView | null> {
  try {
    const f = await b().forwardPort(hostId, remote, local)
    if (local == null && f.localPort !== remote) {
      pushToast(
        {
          kind: 'info',
          title: `localhost:${remote} is taken on this Mac`,
          body: `Forwarded to localhost:${f.localPort} instead.`
        },
        5000
      )
    }
    if (opts.pin) setPins(hostId, (p) => [...p.filter((x) => x.remote !== remote), { remote, local: f.localPort }])
    return f
  } catch (err) {
    reportError(`Could not forward port ${remote}`, err)
    return null
  }
}

/** Stop forwarding (and forget the pin). */
export async function stopForward(hostId: string, remote: number): Promise<void> {
  setPins(hostId, (p) => p.filter((x) => x.remote !== remote))
  try {
    await b().stopForward(hostId, remote)
  } catch (err) {
    if (CivetError.from(err).code !== 'notFound') reportError(`Could not stop forwarding port ${remote}`, err)
  }
}

export function togglePin(hostId: string, f: ForwardView): void {
  if (isPinned(usePrefs.getState().pinnedForwards, hostId, f.remotePort)) {
    setPins(hostId, (p) => p.filter((x) => x.remote !== f.remotePort))
  } else {
    setPins(hostId, (p) => [...p, { remote: f.remotePort, local: f.localPort }])
  }
}

export function localUrl(port: number): string {
  return `http://localhost:${port}`
}

export async function openLocalPort(port: number): Promise<void> {
  try {
    await b().openLocalUrl(localUrl(port))
  } catch (err) {
    reportError(`Could not open ${localUrl(port)}`, err)
  }
}

/** True if a forward through the host's loopback interface can reach it. */
export function reachableFromLoopback(addrs: readonly string[]): boolean {
  return addrs.some((a) => a === '::1' || a === '0.0.0.0' || a === '::' || a.startsWith('127.'))
}
