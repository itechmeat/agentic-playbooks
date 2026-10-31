// Runs before the app bundle (a classic script, so it blocks first paint).
// Dark is the dashboard's default: when no theme was chosen yet, store "dark"
// before mode-watcher creates its state, which would otherwise persist
// "system" and win over ModeWatcher's defaultMode. Then apply the stored
// choice to <html> so a light-theme user gets no dark flash. A separate file
// because the page CSP refuses inline scripts.
;(function () {
  try {
    var key = 'mode-watcher-mode'
    var mode = localStorage.getItem(key)
    if (mode == null) {
      mode = 'dark'
      localStorage.setItem(key, mode)
    }
    var dark =
      mode === 'dark' ||
      (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
    document.documentElement.classList.toggle('dark', dark)
  } catch (e) {
    // Storage blocked: keep the dark class index.html ships.
  }
})()
