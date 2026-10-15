// Ported from Orca: src/renderer/src/lib/pane-manager/pane-terminal-options.ts
// (buildDefaultTerminalOptions) and src/shared/terminal-themes/defaults.ts
// ("Ghostty Default Style Dark"). Settings plumbing, contrast correction and
// the scrollback policy module were dropped; Civet has one fixed theme
// (Ghostty's default colours, font from the Civet Workspace v3 design).
import type { ITerminalOptions, ITheme } from '@xterm/xterm'

export const TERMINAL_THEME_DARK: ITheme = {
  // Ghostty's built-in Tomorrow Night 16-colour palette (`ghostty
  // +show-config --default`) on the app's work-area background (--background
  // in main.css), so the terminal sits flush in the chrome. Cursor and
  // selection follow Ghostty's unset behaviour (foreground cursor, inverted
  // selection).
  background: '#151616',
  foreground: '#e6e6e6',
  cursor: '#ffffff',
  cursorAccent: '#151616',
  selectionBackground: '#ffffff',
  selectionForeground: '#151616',
  black: '#1d1f21',
  red: '#cc6666',
  green: '#b5bd68',
  yellow: '#f0c674',
  blue: '#81a2be',
  magenta: '#b294bb',
  cyan: '#8abeb7',
  white: '#c5c8c6',
  brightBlack: '#666666',
  brightRed: '#d54e53',
  brightGreen: '#b9ca4a',
  brightYellow: '#e7c547',
  brightBlue: '#7aa6da',
  brightMagenta: '#c397d8',
  brightCyan: '#70c0b1',
  brightWhite: '#eaeaea'
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
    // Ghostty default (bold-is-bright = false): bold keeps its own colour.
    drawBoldTextInBrightColors: false,
    scrollbar: { width: 7 },
    theme: TERMINAL_THEME_DARK
  }
}
