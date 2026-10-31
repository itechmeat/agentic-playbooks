import { refreshAuthStatus } from './auth.svelte'
import { checkServerBuild } from './buildcheck'

// Subscribe to server change events over the dashboard WebSocket. Filesystem
// events are chatty and arrive steadily (~every few hundred ms) while a run
// streams, so a plain debounce is wrong twice over: too short and it does not
// coalesce; long enough to coalesce and it withholds every update until the
// run goes quiet (the view looks frozen again). Instead this throttles: the
// first frame fires immediately (real-time feel), then further frames fire at
// most once per `minIntervalMs`. A continuous stream becomes a steady ~1.6
// reloads/sec instead of one per frame; an isolated burst collapses to one.
// Teardown clears any pending trailing call and marks the subscription closed,
// so a late frame never fires after the view unmounts.
export function subscribeChanges(cb: () => void, minIntervalMs = 600): () => void {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`)
  let last = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  let closed = false
  let opened = false
  const fire = () => {
    if (closed) return
    last = performance.now()
    cb()
  }
  ws.onopen = () => {
    opened = true
  }
  // A socket that closes without ever opening was refused at the upgrade, and
  // in server mode the usual reason is an absent or expired session. Re-read
  // the auth status rather than retrying blind against a gate that will keep
  // refusing.
  // A socket that was open and dropped usually means the server restarted,
  // which on a reinstall means a new build: ask which one, once it is back.
  ws.onclose = () => {
    if (closed) return
    if (!opened) void refreshAuthStatus()
    else for (const ms of [2000, 8000]) setTimeout(() => void checkServerBuild(), ms)
  }
  ws.onmessage = () => {
    if (closed) return
    const elapsed = performance.now() - last
    if (elapsed >= minIntervalMs) {
      clearTimeout(timer)
      timer = undefined
      fire()
    } else if (timer === undefined) {
      timer = setTimeout(() => {
        timer = undefined
        fire()
      }, minIntervalMs - elapsed)
    }
  }
  return () => {
    closed = true
    clearTimeout(timer)
    ws.close()
  }
}

/**
 * A slow lane next to `subscribeChanges` for reads too heavy to repeat on
 * every change (the run listing reads every journal). `request()` runs `fn`
 * at once when it last ran at least `minIntervalMs` ago, else once at the end
 * of that window; requests in between collapse into that one call. While the
 * tab is hidden nothing runs: the request waits and fires when the tab is
 * shown again. `dispose()` drops a pending call.
 */
export function slowRefresh(
  fn: () => void,
  minIntervalMs = 10_000,
): { request: () => void; dispose: () => void } {
  let last = Number.NEGATIVE_INFINITY
  let timer: ReturnType<typeof setTimeout> | undefined
  let waitingForVisible = false
  let closed = false
  const hidden = () => typeof document !== 'undefined' && document.visibilityState === 'hidden'
  const run = () => {
    timer = undefined
    if (closed) return
    if (hidden()) {
      waitingForVisible = true
      return
    }
    waitingForVisible = false
    last = performance.now()
    fn()
  }
  const request = () => {
    if (closed || timer !== undefined) return
    const wait = last + minIntervalMs - performance.now()
    if (wait <= 0) run()
    else timer = setTimeout(run, wait)
  }
  const onVisibility = () => {
    if (waitingForVisible && !hidden()) request()
  }
  if (typeof document !== 'undefined') document.addEventListener('visibilitychange', onVisibility)
  return {
    request,
    dispose() {
      closed = true
      clearTimeout(timer)
      if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', onVisibility)
    },
  }
}
