// Rendered Markdown for a file tab (FileView.tsx toggles it with the
// source). GitHub-flavoured (tables, task lists, strikethrough, autolinks);
// raw HTML is not rendered. Links: `http(s)` opens in the browser, other
// worktree files open in a tab, `#heading` scrolls. Images other than
// `data:` show as a placeholder (the CSP blocks remote ones, and worktree
// files are not served to the webview).
import { useEffect, useRef } from 'react'
import { ImageIcon } from 'lucide-react'
import Markdown, { type Components, type UrlTransform, defaultUrlTransform } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { Slugger, isDataImage, isMarkdownPath, resolveLink } from '@/lib/markdown'
import { fileKey, setPendingAnchor, useFiles } from '@/state/files'
import { openFileTab, pushToast, useStore } from '@/state/store'

export function MarkdownView({
  hostId,
  worktree,
  path,
  text
}: {
  hostId: string
  worktree: string
  path: string
  text: string
}) {
  const root = useRef<HTMLDivElement>(null)
  const key = fileKey(hostId, worktree, path)
  const pending = useFiles((s) => s.pendingAnchor[key])

  const scrollTo = (id: string) =>
    root.current
      ?.querySelector(`[data-anchor="${CSS.escape(id)}"]`)
      ?.scrollIntoView({ block: 'start' })

  // A link from another file (`this.md#x`) left a heading to scroll to.
  // biome-ignore lint/correctness/useExhaustiveDependencies: rerun when the text renders
  useEffect(() => {
    if (!pending) return
    scrollTo(pending)
    setPendingAnchor(key, null)
  }, [pending, key, text])

  const follow = (href: string) => {
    const link = resolveLink(path, href)
    if (!link) {
      pushToast({ kind: 'info', title: `Can't open ${href}` }, 3200)
    } else if (link.kind === 'web') {
      const be = useStore.getState().backend
      void be?.openWebUrl(link.url).catch((err: Error) => {
        pushToast({ kind: 'error', title: 'Could not open link', body: err.message }, 6000)
      })
    } else if (link.kind === 'anchor') {
      scrollTo(link.id)
    } else {
      if (link.hash && isMarkdownPath(link.path)) {
        setPendingAnchor(fileKey(hostId, worktree, link.path), link.hash)
      }
      openFileTab(hostId, worktree, link.path)
    }
  }

  const components: Components = {
    a: ({ href, children, node: _, ...rest }) => (
      <a
        {...rest}
        href={href}
        onClick={(e) => {
          e.preventDefault()
          if (href) follow(href)
        }}
      >
        {children}
      </a>
    ),
    img: ({ src, alt }) => {
      if (typeof src === 'string' && isDataImage(src)) {
        return <img src={src} alt={alt ?? ''} />
      }
      const target = typeof src === 'string' ? resolveLink(path, src) : null
      return (
        <span className="md-image-placeholder">
          <ImageIcon />
          {target?.kind === 'file' ? (
            <button type="button" onClick={() => openFileTab(hostId, worktree, target.path)}>
              {alt || target.path}
            </button>
          ) : (
            <span>{alt || src || 'image'}</span>
          )}
        </span>
      )
    },
    // Task-list checkboxes are read-only.
    input: ({ node: _, ...rest }) => <input {...rest} disabled />
  }

  return (
    <div ref={root} className="min-h-0 flex-1 overflow-auto">
      <div className="markdown-body mx-auto max-w-[860px] px-8 py-6 select-text">
        <Markdown
          remarkPlugins={[remarkGfm]}
          rehypePlugins={[rehypeHeadingAnchors]}
          components={components}
          urlTransform={urlTransform}
        >
          {text}
        </Markdown>
      </div>
    </div>
  )
}

/** The default (safe protocols and relative URLs), plus raster `data:` images. */
const urlTransform: UrlTransform = (url, key, node) =>
  key === 'src' && node.tagName === 'img' && isDataImage(url) ? url : defaultUrlTransform(url)

/** The bits of a hast node the anchor plugin reads. */
type HastNode = {
  type: string
  tagName?: string
  value?: string
  properties?: Record<string, unknown>
  children?: HastNode[]
}

/** `data-anchor` on every heading (GitHub-style slug) for `#heading` links. */
function rehypeHeadingAnchors() {
  return (tree: HastNode) => {
    const slugger = new Slugger()
    const walk = (node: HastNode) => {
      if (node.type === 'element' && /^h[1-6]$/.test(node.tagName ?? '')) {
        node.properties = { ...node.properties, dataAnchor: slugger.slug(textOf(node)) }
      }
      node.children?.forEach(walk)
    }
    walk(tree)
  }
}

function textOf(node: HastNode): string {
  if (node.type === 'text') return node.value ?? ''
  return (node.children ?? []).map(textOf).join('')
}
