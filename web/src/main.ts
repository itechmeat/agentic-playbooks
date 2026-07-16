import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'

// Light/dark follows the OS setting via mode-watcher's <ModeWatcher /> mounted
// in App.svelte: it owns the `.dark` class on the root and also feeds the
// shared `mode` store that svelte-sonner reads, so toasts theme correctly too.
const app = mount(App, {
  target: document.getElementById('app')!,
})

export default app
