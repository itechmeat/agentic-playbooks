import type { WfEvent } from './types'

export interface PendingReview {
  node: string
  options: string[]
}

// human_review nodes awaiting a decision: they have a review_requested event
// without a subsequent review_decided (by comparing event counts of each kind).
export function pendingReviews(events: WfEvent[]): PendingReview[] {
  const requested = new Map<string, { count: number; options: string[] }>()
  const decided = new Map<string, number>()
  for (const e of events) {
    if (e.type === 'review_requested' && e.node) {
      const prev = requested.get(e.node)
      const raw = e.options
      const options = Array.isArray(raw) ? (raw as string[]) : []
      requested.set(e.node, { count: (prev?.count ?? 0) + 1, options })
    } else if (e.type === 'review_decided' && e.node) {
      decided.set(e.node, (decided.get(e.node) ?? 0) + 1)
    }
  }
  const out: PendingReview[] = []
  for (const [node, { count, options }] of requested) {
    if (count > (decided.get(node) ?? 0)) {
      out.push({ node, options })
    }
  }
  return out
}
