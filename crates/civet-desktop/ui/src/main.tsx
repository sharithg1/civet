import { createRoot } from 'react-dom/client'
import '@xterm/xterm/css/xterm.css'
import './assets/main.css'
import { App } from './App'

// No StrictMode: its dev double-mount would attach/detach every pane twice.
createRoot(document.getElementById('root')!).render(<App />)
