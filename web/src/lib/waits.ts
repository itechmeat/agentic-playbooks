import type { WfEvent } from './types'

// wait nodes awaiting a signal/timer: there's a wait_started without a
// subsequent wait_signalled/wait_timeout (by comparing event counts of each kind).
export function pendingWaits(events: WfEvent[]): string[] {
  const started = new Map<string, number>()
  const ended = new Map<string, number>()
  for (const e of events) {
    if (!e.node) continue
    if (e.type === 'wait_started') {
      started.set(e.node, (started.get(e.node) ?? 0) + 1)
    } else if (e.type === 'wait_signalled' || e.type === 'wait_timeout') {
      ended.set(e.node, (ended.get(e.node) ?? 0) + 1)
    }
  }
  const out: string[] = []
  for (const [node, count] of started) {
    if (count > (ended.get(node) ?? 0)) out.push(node)
  }
  return out
}
