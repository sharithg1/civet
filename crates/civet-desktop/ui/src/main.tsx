import { createRoot } from 'react-dom/client'
import '@xterm/xterm/css/xterm.css'
import './assets/main.css'
import { App } from './App'
import { TooltipProvider } from './components/ui/tooltip'

// No StrictMode: its dev double-mount would attach/detach every pane twice.
createRoot(document.getElementById('root')!).render(
  // One provider for every tooltip (icon buttons); a short delay so sweeping
  // across the tab bar does not flash labels.
  <TooltipProvider delayDuration={400}>
    <App />
  </TooltipProvider>
)
