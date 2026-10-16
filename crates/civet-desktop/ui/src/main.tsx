import { createRoot } from 'react-dom/client'
import '@xterm/xterm/css/xterm.css'
// Bundled webfonts (offline-safe in the Tauri app): Geist for the chrome,
// Geist Mono for terminals and mono labels.
import '@fontsource-variable/geist'
import '@fontsource-variable/geist-mono'
import './assets/main.css'
import { isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { App } from './App'
import { AboutWindow } from './components/AboutWindow'
import { TooltipProvider } from './components/ui/tooltip'

// The About window (src/updater.rs) loads this same bundle; `?about` previews
// it in a browser.
const isAbout = isTauri()
  ? getCurrentWindow().label === 'about'
  : new URLSearchParams(location.search).has('about')

// xterm measures its cell size once when a terminal opens, so the terminal
// font must be loaded before the first pane mounts. Bounded wait: a missing
// font must not block the app.
const fontsReady = Promise.race([
  Promise.all([
    document.fonts.load('350 13px "Geist Mono Variable"'),
    document.fonts.load('500 13px "Geist Mono Variable"'),
    document.fonts.load('400 13px "Geist Variable"')
  ]),
  new Promise((r) => setTimeout(r, 1500))
]).catch(() => {})

// No StrictMode: its dev double-mount would attach/detach every pane twice.
void fontsReady.then(() =>
  createRoot(document.getElementById('root')!).render(
    // One provider for every tooltip (icon buttons); a short delay so sweeping
    // across the tab bar does not flash labels.
    <TooltipProvider delayDuration={400}>{isAbout ? <AboutWindow /> : <App />}</TooltipProvider>
  )
)
