// Code text with syntax highlighting (lib/highlight.ts), for a <pre> or
// <code> with the `shiki-tokens` class. Shows the plain text until the
// tokens are ready (and for text that isn't highlighted), so there is no
// blank flash and the layout doesn't move.
import { Fragment, useEffect, useState } from 'react'
import { type HighlightLines, highlight } from '@/lib/highlight'

export function HighlightedText({ text, lang }: { text: string; lang: string | null }) {
  const lines = useHighlight(text, lang)
  if (!lines) return text
  return lines.map((line, i) => (
    <Fragment key={i}>
      {i > 0 && '\n'}
      {line.map((t, j) => (
        <span key={j} style={t.style}>
          {t.content}
        </span>
      ))}
    </Fragment>
  ))
}

/** `text`'s tokens once highlighted; null before that or if it isn't. */
function useHighlight(text: string, lang: string | null): HighlightLines | null {
  const [done, setDone] = useState<{ text: string; lang: string; lines: HighlightLines }>()
  useEffect(() => {
    if (!lang) return
    let live = true
    void highlight(text, lang).then((lines) => {
      if (live && lines) setDone({ text, lang, lines })
    })
    return () => {
      live = false
    }
  }, [text, lang])
  return done && done.text === text && done.lang === lang ? done.lines : null
}
