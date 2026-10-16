// In-app updates and version info (src/updater.rs on the Rust side). Used
// by the About window and the main window's "Restart to update" pill.
// Outside Tauri (mock UI in a browser) everything is a canned stand-in.

import { invoke, isTauri } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { useEffect, useState } from 'react'

export type UpdatePhase = 'checking' | 'upToDate' | 'downloading' | 'ready' | 'error'

/** `civet://update` payload. */
export type UpdateEvent = {
  phase: UpdatePhase
  current: string
  version: string | null
  /** Started by the user (menu, About window), not the periodic check. */
  manual: boolean
  message: string | null
}

export type AppInfo = {
  version: string
  arch: string
  /** Empty when unknown. */
  commit: string
  /** RFC 3339; empty when unknown. */
  commitDate: string
  os: string
}

export const UPDATE_EVENT = 'civet://update'

const mockInfo: AppInfo = {
  version: '0.0.0-mock',
  arch: 'aarch64',
  commit: '',
  commitDate: new Date().toISOString(),
  os: 'browser'
}

export function appInfo(): Promise<AppInfo> {
  return isTauri() ? invoke<AppInfo>('app_info') : Promise.resolve(mockInfo)
}

export function checkForUpdates(): Promise<UpdateEvent> {
  if (!isTauri()) {
    return Promise.resolve({
      phase: 'upToDate',
      current: mockInfo.version,
      version: null,
      manual: true,
      message: null
    })
  }
  return invoke<UpdateEvent>('check_for_updates')
}

/** Install the downloaded update and relaunch; resolves only on failure. */
export function installUpdate(): Promise<void> {
  return isTauri() ? invoke<void>('install_update') : Promise.resolve()
}

/** The current update state: the last event, then every new one. */
export function useUpdateStatus(): UpdateEvent | null {
  const [status, setStatus] = useState<UpdateEvent | null>(null)
  useEffect(() => {
    if (!isTauri()) return
    let cancelled = false
    let unlisten: (() => void) | undefined
    void listen<UpdateEvent>(UPDATE_EVENT, (e) => setStatus(e.payload)).then((u) => {
      if (cancelled) u()
      else unlisten = u
    })
    void invoke<UpdateEvent | null>('update_status').then((s) => {
      if (!cancelled && s) setStatus((cur) => cur ?? s)
    })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])
  return status
}

const ARCH_LABEL: Record<string, string> = { aarch64: 'Apple Silicon', x86_64: 'Intel' }

export function archLabel(arch: string): string {
  return ARCH_LABEL[arch] ?? arch
}

/** "Version 0.1.12 (Apple Silicon)" plus commit and OS, for bug reports. */
export function versionInfoText(info: AppInfo): string {
  return [
    `Civet ${info.version} (${archLabel(info.arch)})`,
    info.commit && `Commit: ${info.commit}`,
    info.commitDate && `Date: ${info.commitDate}`,
    `OS: ${info.os}`
  ]
    .filter(Boolean)
    .join('\n')
}

/** "Oct 14, 2026 (2 days ago)". */
export function releaseDateLabel(iso: string, now = new Date()): string {
  const d = new Date(iso)
  if (!iso || Number.isNaN(d.getTime())) return ''
  const date = d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })
  const days = Math.floor((now.getTime() - d.getTime()) / 86_400_000)
  const ago = days <= 0 ? 'today' : days === 1 ? '1 day ago' : `${days} days ago`
  return `${date} (${ago})`
}
