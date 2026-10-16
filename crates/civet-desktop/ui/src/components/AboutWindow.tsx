// The About window (Civet → About Civet / Check for Updates…): version,
// release date, update check and "Copy version info". Its own Tauri window
// (label "about", see src/updater.rs) rendering this instead of <App/>.
import { useEffect, useState } from 'react'
import { Check, CornerDownLeft } from 'lucide-react'
import { isTauri, invoke } from '@tauri-apps/api/core'
import icon from '@/assets/civet-icon.png'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import {
  type AppInfo,
  appInfo,
  archLabel,
  checkForUpdates,
  installUpdate,
  releaseDateLabel,
  useUpdateStatus,
  versionInfoText
} from '@/updates'

export function AboutWindow() {
  const [info, setInfo] = useState<AppInfo | null>(null)
  const [copied, setCopied] = useState(false)
  const [installError, setInstallError] = useState<string | null>(null)
  const [installing, setInstalling] = useState(false)
  const status = useUpdateStatus()

  useEffect(() => {
    void appInfo().then(setInfo)
  }, [])

  const copy = async () => {
    if (!info) return
    const text = versionInfoText(info)
    try {
      if (isTauri()) await invoke('write_clipboard', { text })
      else await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      // Nothing useful to show in a window this small.
    }
  }

  const install = async () => {
    setInstalling(true)
    setInstallError(null)
    try {
      await installUpdate()
    } catch (err) {
      setInstallError((err as { message?: string })?.message ?? String(err))
      setInstalling(false)
    }
  }

  // Return copies, as the ↵ on the button says.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Enter' && !e.repeat && !(e.target instanceof HTMLButtonElement)) void copy()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  const busy = status?.phase === 'checking' || status?.phase === 'downloading'
  const ready = status?.phase === 'ready'
  let line: string | null = null
  if (installError) line = `Could not install the update: ${installError}`
  else if (status?.phase === 'checking') line = 'Checking for updates…'
  else if (status?.phase === 'downloading') line = `Downloading Civet ${status.version}…`
  else if (ready) line = `Civet ${status.version} is ready to install.`
  else if (status?.phase === 'upToDate') line = 'Civet is up to date.'
  else if (status?.phase === 'error') line = `Update check failed: ${status.message}`

  return (
    <div className="flex h-full w-full flex-col items-center bg-card px-6 pt-9 pb-5 text-center select-none">
      <div data-tauri-drag-region className="absolute inset-x-0 top-0 h-8" />
      <img src={icon} alt="" className="size-16" draggable={false} />
      <div className="mt-3 text-xl font-semibold text-foreground">Civet</div>
      <div className="mt-1 text-base text-soft">
        {info ? `Version ${info.version} (${archLabel(info.arch)})` : ' '}
      </div>
      <div className="mt-1 text-xs text-dim">
        {info ? releaseDateLabel(info.commitDate) || ' ' : ' '}
      </div>
      <div className="mt-5 flex gap-2">
        {ready ? (
          <Button size="md" disabled={installing} onClick={() => void install()}>
            {installing && <Spinner className="size-3.5" />}
            Restart to Update
          </Button>
        ) : (
          <Button
            size="md"
            variant="outline"
            disabled={busy}
            onClick={() => void checkForUpdates()}
          >
            {busy && <Spinner className="size-3.5" />}
            Check for Updates…
          </Button>
        )}
        <Button
          size="md"
          variant={ready ? 'outline' : 'default'}
          disabled={!info}
          onClick={() => void copy()}
        >
          {copied ? 'Copied' : 'Copy version info'}
          {copied ? <Check className="size-3.5" /> : <CornerDownLeft className="size-3.5" />}
        </Button>
      </div>
      <div
        className="mt-3 min-h-[36px] max-w-full text-xs leading-[18px] break-words text-muted-foreground"
        role="status"
      >
        {line}
      </div>
    </div>
  )
}
