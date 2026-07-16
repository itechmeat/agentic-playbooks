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
  const fire = () => {
    if (closed) return
    last = performance.now()
    cb()
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
