// Hosts the user connected to, persisted in localStorage so the next launch
// reconnects them instead of showing the connect screen. Added on a successful
// connect, removed on disconnect; a failed launch-time reconnect keeps the
// entry (the host shows as failed with a Reconnect action).

import type { HostTarget, TransportChoice } from '@/types'

export type SavedHost = { hostId: string; target: HostTarget; transport: TransportChoice }

const KEY = 'civet.hosts.v1'

export function loadSavedHosts(): SavedHost[] {
  try {
    const raw = typeof localStorage !== 'undefined' ? localStorage.getItem(KEY) : null
    const list = raw ? (JSON.parse(raw) as unknown) : []
    return Array.isArray(list) ? (list as SavedHost[]).filter((h) => h && h.hostId && h.target) : []
  } catch {
    return []
  }
}

function store(list: SavedHost[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(list))
  } catch {
    // Storage unavailable (private mode): hosts are not remembered.
  }
}

export function savedHost(hostId: string): SavedHost | undefined {
  return loadSavedHosts().find((h) => h.hostId === hostId)
}

export function saveHost(entry: SavedHost): void {
  const list = loadSavedHosts().filter((h) => h.hostId !== entry.hostId)
  store([...list, entry])
}

export function forgetHost(hostId: string): void {
  store(loadSavedHosts().filter((h) => h.hostId !== hostId))
}
