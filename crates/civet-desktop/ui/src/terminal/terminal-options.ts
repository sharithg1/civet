// Ported from Orca: src/renderer/src/lib/pane-manager/pane-terminal-options.ts
// (buildDefaultTerminalOptions) and src/shared/terminal-themes/defaults.ts
// ("Ghostty Default Style Dark"). Settings plumbing, contrast correction and
// the scrollback policy module were dropped; Civet has one fixed theme
// (colours and font from the Civet Workspace v3 design).
import type { ITerminalOptions, ITheme } from '@xterm/xterm'

export const TERMINAL_THEME_DARK: ITheme = {
  // Civet v3: near-black content area, neutral text, the design's oklch
  // prompt/status hues converted to hex (xterm parses hex/rgb only).
  background: '#0a0a0a',
  foreground: '#e5e5e5',
  cursor: '#d4d4d4',
  cursorAccent: '#0a0a0a',
  selectionBackground: 'rgba(120, 160, 255, 0.3)',
  selectionForeground: '#fafafa',
  black: '#262626',
  red: '#ef6661',
  green: '#c2c564',
  yellow: '#ebb25f',
  blue: '#7fb6ee',
  magenta: '#af95df',
  cyan: '#64c3c3',
  white: '#d4d4d4',
  brightBlack: '#737373',
  brightRed: '#fb9890',
  brightGreen: '#65c98c',
  brightYellow: '#f2af48',
  brightBlue: '#8bbcef',
  brightMagenta: '#c3acec',
  brightCyan: '#8fd6d6',
  brightWhite: '#fafafa'
}

/** Bundled Geist Mono first (see main.tsx: loaded before any terminal opens). */
export const TERMINAL_FONT_FAMILY =
  '"Geist Mono Variable", "SF Mono", "Menlo", "Monaco", "Cascadia Mono", "Consolas", "DejaVu Sans Mono", "Liberation Mono", "Symbols Nerd Font Mono", "MesloLGS Nerd Font", "JetBrainsMono Nerd Font", "Hack Nerd Font", monospace'

/**
 * Local xterm scrollback. The daemon retains 10 000 lines per pane and a Full
 * replays the newest 1000, so this only needs to hold what was replayed live;
 * older lines come from fetch_scrollback.
 */
export const TERMINAL_SCROLLBACK_ROWS = 10_000

export function buildDefaultTerminalOptions(): ITerminalOptions {
  return {
    allowProposedApi: true,
    cursorBlink: true,
    cursorStyle: 'block',
    cursorInactiveStyle: 'outline',
    fontSize: 13,
    fontFamily: TERMINAL_FONT_FAMILY,
    fontWeight: '400',
    fontWeightBold: '600',
    scrollback: TERMINAL_SCROLLBACK_ROWS,
    scrollSensitivity: 1.15,
    fastScrollSensitivity: 5,
    allowTransparency: false,
    // Why (Orca): on macOS, non-US layouts rely on Option to compose characters like @ and €.
    macOptionIsMeta: false,
    macOptionClickForcesSelection: true,
    drawBoldTextInBrightColors: true,
    scrollbar: { width: 7 },
    theme: TERMINAL_THEME_DARK
  }
}
