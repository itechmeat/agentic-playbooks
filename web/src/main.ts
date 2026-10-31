import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'

// Dark by default (index.html ships `class="dark"` and public/theme-init.js
// seeds the stored choice), then the Topbar switch
// (dark / light / system) via mode-watcher's <ModeWatcher /> mounted
// in App.svelte: it owns the `.dark` class on the root and also feeds the
// shared `mode` store that svelte-sonner reads, so toasts theme correctly too.
const app = mount(App, {
  target: document.getElementById('app')!,
})

export default app
