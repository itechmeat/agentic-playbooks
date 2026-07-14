export function subscribeChanges(cb: () => void): () => void {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`)
  ws.onmessage = () => cb()
  return () => ws.close()
}
