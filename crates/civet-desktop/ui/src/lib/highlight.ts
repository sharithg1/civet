// Syntax highlighting for file tabs (FileView.tsx) and Markdown code fences
// (MarkdownView.tsx), with Shiki. Shiki, its grammars and themes are
// imported on first use, one chunk per language, so none of it is in the
// main bundle. Tokens carry both themes' colours as CSS variables
// (`--shiki-light` / `--shiki-dark`, picked by `.shiki-tokens` in main.css).

import type { HighlighterCore, LanguageRegistration } from 'shiki/core'

/** Text longer than this (characters) stays plain: tokenizing is slow and
 * the result is a span per token. */
export const MAX_HIGHLIGHT_CHARS = 100 * 1024

type Grammar = () => Promise<{ default: LanguageRegistration[] }>

/** The languages we highlight, by Shiki id. */
const GRAMMARS: Record<string, Grammar> = {
  c: () => import('shiki/langs/c.mjs'),
  cpp: () => import('shiki/langs/cpp.mjs'),
  csharp: () => import('shiki/langs/csharp.mjs'),
  css: () => import('shiki/langs/css.mjs'),
  dart: () => import('shiki/langs/dart.mjs'),
  diff: () => import('shiki/langs/diff.mjs'),
  docker: () => import('shiki/langs/docker.mjs'),
  elixir: () => import('shiki/langs/elixir.mjs'),
  fish: () => import('shiki/langs/fish.mjs'),
  go: () => import('shiki/langs/go.mjs'),
  graphql: () => import('shiki/langs/graphql.mjs'),
  haskell: () => import('shiki/langs/haskell.mjs'),
  hcl: () => import('shiki/langs/hcl.mjs'),
  html: () => import('shiki/langs/html.mjs'),
  ini: () => import('shiki/langs/ini.mjs'),
  java: () => import('shiki/langs/java.mjs'),
  javascript: () => import('shiki/langs/javascript.mjs'),
  json: () => import('shiki/langs/json.mjs'),
  json5: () => import('shiki/langs/json5.mjs'),
  jsonc: () => import('shiki/langs/jsonc.mjs'),
  jsx: () => import('shiki/langs/jsx.mjs'),
  kotlin: () => import('shiki/langs/kotlin.mjs'),
  less: () => import('shiki/langs/less.mjs'),
  lua: () => import('shiki/langs/lua.mjs'),
  make: () => import('shiki/langs/make.mjs'),
  markdown: () => import('shiki/langs/markdown.mjs'),
  nix: () => import('shiki/langs/nix.mjs'),
  php: () => import('shiki/langs/php.mjs'),
  powershell: () => import('shiki/langs/powershell.mjs'),
  proto: () => import('shiki/langs/proto.mjs'),
  python: () => import('shiki/langs/python.mjs'),
  ruby: () => import('shiki/langs/ruby.mjs'),
  rust: () => import('shiki/langs/rust.mjs'),
  scala: () => import('shiki/langs/scala.mjs'),
  scss: () => import('shiki/langs/scss.mjs'),
  shellscript: () => import('shiki/langs/shellscript.mjs'),
  sql: () => import('shiki/langs/sql.mjs'),
  svelte: () => import('shiki/langs/svelte.mjs'),
  swift: () => import('shiki/langs/swift.mjs'),
  terraform: () => import('shiki/langs/terraform.mjs'),
  toml: () => import('shiki/langs/toml.mjs'),
  tsx: () => import('shiki/langs/tsx.mjs'),
  typescript: () => import('shiki/langs/typescript.mjs'),
  vue: () => import('shiki/langs/vue.mjs'),
  xml: () => import('shiki/langs/xml.mjs'),
  yaml: () => import('shiki/langs/yaml.mjs'),
  zig: () => import('shiki/langs/zig.mjs')
}

/** File extensions and fence names that aren't a Shiki id. */
const ALIASES: Record<string, string> = {
  bash: 'shellscript',
  cc: 'cpp',
  cjs: 'javascript',
  cs: 'csharp',
  cts: 'typescript',
  cxx: 'cpp',
  dockerfile: 'docker',
  ex: 'elixir',
  exs: 'elixir',
  h: 'c',
  hh: 'cpp',
  hpp: 'cpp',
  hs: 'haskell',
  htm: 'html',
  js: 'javascript',
  kt: 'kotlin',
  kts: 'kotlin',
  makefile: 'make',
  markdown: 'markdown',
  md: 'markdown',
  mjs: 'javascript',
  mk: 'make',
  mts: 'typescript',
  plist: 'xml',
  ps1: 'powershell',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  sh: 'shellscript',
  shell: 'shellscript',
  svg: 'xml',
  tf: 'terraform',
  ts: 'typescript',
  yml: 'yaml',
  zsh: 'shellscript'
}

/** Files known by name rather than extension (lower-cased basename). */
const FILE_NAMES: Record<string, string> = {
  '.bashrc': 'shellscript',
  '.bash_profile': 'shellscript',
  '.gitconfig': 'ini',
  '.profile': 'shellscript',
  '.zprofile': 'shellscript',
  '.zshrc': 'shellscript',
  'cargo.lock': 'toml',
  containerfile: 'docker',
  dockerfile: 'docker',
  gnumakefile: 'make',
  makefile: 'make'
}

function resolve(name: string): string | null {
  const n = name.toLowerCase()
  const id = ALIASES[n] ?? n
  return Object.hasOwn(GRAMMARS, id) ? id : null
}

/** The Shiki language for a file, by name or extension; null if we have none. */
export function languageForPath(path: string): string | null {
  const base = (path.split('/').pop() ?? '').toLowerCase()
  if (Object.hasOwn(FILE_NAMES, base)) return FILE_NAMES[base] ?? null
  if (base.startsWith('dockerfile.')) return 'docker'
  const dot = base.lastIndexOf('.')
  return dot > 0 ? resolve(base.slice(dot + 1)) : null
}

/** The Shiki language for a Markdown fence's info string (```ts title=x). */
export function languageForFence(info: string): string | null {
  // Pandoc-style `{.python}` too.
  const word = /^\{?\.?([^\s}]+)/.exec(info.trim())?.[1]
  return word ? resolve(word) : null
}

export type HighlightToken = {
  content: string
  /** `--shiki-light` / `--shiki-dark` (and `-font-style` …) variables. */
  style?: Record<string, string>
}

/** Tokens per line (lines split on `\n`, without it). */
export type HighlightLines = HighlightToken[][]

let highlighter: Promise<HighlighterCore> | null = null
const loaded = new Map<string, Promise<void>>()

function getHighlighter(): Promise<HighlighterCore> {
  highlighter ??= (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }, light, dark] =
      await Promise.all([
        import('shiki/core'),
        import('shiki/engine/javascript'),
        import('shiki/themes/github-light-default.mjs'),
        import('shiki/themes/github-dark-default.mjs')
      ])
    return createHighlighterCore({
      themes: [light.default, dark.default],
      engine: createJavaScriptRegexEngine()
    })
  })()
  highlighter.catch(() => {
    highlighter = null
  })
  return highlighter
}

/**
 * `text` tokenized as `lang` (from languageForPath / languageForFence), or
 * null when it's too long or the language fails to load.
 */
export async function highlight(text: string, lang: string): Promise<HighlightLines | null> {
  const grammar = GRAMMARS[lang]
  if (!grammar || text.length > MAX_HIGHLIGHT_CHARS) return null
  try {
    const hl = await getHighlighter()
    let ready = loaded.get(lang)
    if (!ready) {
      ready = grammar().then((m) => hl.loadLanguage(m.default))
      loaded.set(lang, ready)
      ready.catch(() => loaded.delete(lang))
    }
    await ready
    const { tokens } = hl.codeToTokens(text, {
      lang,
      themes: { light: 'github-light-default', dark: 'github-dark-default' },
      defaultColor: false
    })
    return tokens.map((line) => line.map((t) => ({ content: t.content, style: t.htmlStyle })))
  } catch {
    return null
  }
}
