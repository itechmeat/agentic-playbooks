import type { WfEvent } from './types'

export interface PendingReview {
  node: string
  options: string[]
  // Optional guidance from the gate node's `prompt:` field (issue #102.9),
  // shown above the options in the review panel. Absent on old events and on
  // a gate with no prompt configured.
  prompt?: string | null
}

// human_review nodes awaiting a decision: they have a review_requested event
// without a subsequent review_decided (by comparing event counts of each kind).
export function pendingReviews(events: WfEvent[]): PendingReview[] {
  const requested = new Map<string, { count: number; options: string[]; prompt: string | null }>()
  const decided = new Map<string, number>()
  for (const e of events) {
    if (e.type === 'review_requested' && e.node) {
      const prev = requested.get(e.node)
      const raw = e.options
      const options = Array.isArray(raw) ? (raw as string[]) : []
      const prompt = typeof e.prompt === 'string' ? e.prompt : null
      requested.set(e.node, { count: (prev?.count ?? 0) + 1, options, prompt })
    } else if (e.type === 'review_decided' && e.node) {
      decided.set(e.node, (decided.get(e.node) ?? 0) + 1)
    }
  }
  const out: PendingReview[] = []
  for (const [node, { count, options, prompt }] of requested) {
    if (count > (decided.get(node) ?? 0)) {
      out.push(prompt ? { node, options, prompt } : { node, options })
    }
  }
  return out
}
