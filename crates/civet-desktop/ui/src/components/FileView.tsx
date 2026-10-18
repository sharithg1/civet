// A file tab's contents (opened from the Files tab of the right panel): the
// first 256 KB of the file, read-only, with line numbers and syntax
// highlighting by extension (lib/highlight.ts; files over 100 KB stay
// plain); Markdown files are rendered (MarkdownView.tsx), with a toggle to
// their source. Reloaded when the tab is shown, when the window regains
// focus and on the reload button.
import { useEffect, useMemo } from 'react'
import { Code, Copy, Eye, RotateCw } from 'lucide-react'
import type { FilePreview } from '@/types'
import { pushToast, useStore } from '@/state/store'
import { languageForPath } from '@/lib/highlight'
import { isMarkdownPath } from '@/lib/markdown'
import { PREVIEW_BYTES, fileKey, loadPreview, toggleSource, useFiles } from '@/state/files'
import { useWorktreeInfo } from './chrome'
import { HighlightedText } from './HighlightedText'
import { IconButton } from './IconButton'
import { MarkdownView } from './MarkdownView'

/** Window focus reloads at most this often. */
const FOCUS_REFRESH_MS = 2000

export function FileView({
  hostId,
  worktree,
  path
}: {
  hostId: string
  worktree: string
  path: string
}) {
  const key = fileKey(hostId, worktree, path)
  const file = useFiles((s) => s.previews[key])
  const markdown = isMarkdownPath(path)
  const source = useFiles((s) => !markdown || !!s.showSource[key])
  const connected = useStore((s) => s.hosts[hostId]?.state === 'connected')
  const root = useWorktreeInfo(hostId, worktree)?.info.path

  useEffect(() => {
    if (!connected) return
    void loadPreview(hostId, worktree, path, true)
    let last = Date.now()
    const onFocus = () => {
      if (Date.now() - last < FOCUS_REFRESH_MS) return
      last = Date.now()
      void loadPreview(hostId, worktree, path, true)
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [hostId, worktree, path, connected])

  const copyPath = () => {
    const text = root ? `${root.replace(/\/+$/, '')}/${path}` : path
    void navigator.clipboard?.writeText(text).catch(() => {})
    pushToast({ kind: 'info', title: `Copied ${text}` }, 3200)
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-1 border-b border-border pr-1.5 pl-3">
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-soft" title={path}>
          {path}
        </span>
        {file?.status === 'ok' && (
          <span className="shrink-0 text-xs text-quiet">{formatBytes(file.value.size)}</span>
        )}
        {markdown && (
          <IconButton
            label={source ? 'Show preview' : 'Show source'}
            onClick={() => toggleSource(key)}
          >
            {source ? <Eye /> : <Code />}
          </IconButton>
        )}
        <IconButton
          label="Reload"
          disabled={!connected}
          onClick={() => void loadPreview(hostId, worktree, path, true)}
        >
          <RotateCw />
        </IconButton>
        <IconButton label="Copy path" onClick={copyPath}>
          <Copy />
        </IconButton>
      </div>
      {!file || file.status === 'loading' ? (
        <Message>{connected ? 'Loading…' : 'Not connected.'}</Message>
      ) : file.status === 'error' ? (
        <Message error>{file.message}</Message>
      ) : (
        <FileBody
          file={file.value}
          lang={languageForPath(path)}
          render={
            source
              ? undefined
              : (text) => (
                  <MarkdownView hostId={hostId} worktree={worktree} path={path} text={text} />
                )
          }
        />
      )}
    </div>
  )
}

function FileBody({
  file,
  lang,
  render
}: {
  file: FilePreview
  lang: string | null
  /** Shows the text instead of the numbered source (Markdown preview). */
  render?: (text: string) => React.ReactNode
}) {
  const lines = useMemo(() => {
    const text = file.text.endsWith('\n') ? file.text.slice(0, -1) : file.text
    return text.split('\n').length
  }, [file.text])
  if (file.binary) {
    return <Message>Binary file ({formatBytes(file.size)}); not shown.</Message>
  }
  const notice = file.truncated && (
    <div className="shrink-0 border-b border-border px-3 py-1.5 text-xs text-faint">
      Showing the first {formatBytes(PREVIEW_BYTES)} of {formatBytes(file.size)}.
    </div>
  )
  if (render) {
    return (
      <>
        {notice}
        {render(file.text)}
      </>
    )
  }
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      {notice}
      <div className="flex min-w-max py-2 font-mono text-sm leading-[20px] select-text">
        <pre className="sticky left-0 shrink-0 bg-background pr-4 pl-3 text-right text-dim select-none">
          {Array.from({ length: lines }, (_, i) => i + 1).join('\n')}
        </pre>
        <pre className="shiki-tokens pr-4 text-soft">
          <HighlightedText text={file.text} lang={lang} />
        </pre>
      </div>
    </div>
  )
}

function Message({ children, error }: { children: React.ReactNode; error?: boolean }) {
  return (
    <div
      className={`grid flex-1 place-items-center text-sm ${error ? 'text-status-err' : 'text-quiet'}`}
    >
      {children}
    </div>
  )
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`
  return `${(n / (1024 * 1024)).toFixed(1)} MB`
}
