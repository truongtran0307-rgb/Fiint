import './api'
import '@fontsource/inter/400.css'
import '@fontsource/inter/500.css'
import '@fontsource/inter/600.css'
import '@fontsource/inter/700.css'
import '@fontsource/jetbrains-mono/400.css'
import '@fontsource/jetbrains-mono/600.css'
import '@xterm/xterm/css/xterm.css'
import './styles.css'
import ReactDOM from 'react-dom/client'
import App from './App'
import { useStore } from './store'
import * as actions from './actions'
import { readScreen } from './terminals'

// Handy from DevTools (Ctrl+Shift+I) and for scripted UI checks.
;(window as unknown as { __studio: unknown }).__studio = { useStore, actions, readScreen }

// xterm measures glyphs once at open — make sure the mono font is ready first.
Promise.race([
  Promise.all([document.fonts.load('13px "JetBrains Mono"'), document.fonts.load('600 13px "JetBrains Mono"')]),
  new Promise((r) => setTimeout(r, 1500))
]).finally(() => {
  // No StrictMode: double-mounted effects would spawn duplicate Claude processes.
  ReactDOM.createRoot(document.getElementById('root')!).render(<App />)
})
