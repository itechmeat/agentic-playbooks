import type { WfEvent } from './types'

// Node ids whose events include a node_cache_hit: the node reused a prior
// run's result instead of re-executing (spec: node cache). Pure derivation
// from the event log, mirroring pendingWaits/pendingReviews/interventionJournal.
export function cachedNodeIds(events: WfEvent[]): Set<string> {
  const ids = new Set<string>()
  for (const e of events) {
    if (e.type === 'node_cache_hit' && e.node) ids.add(e.node)
  }
  return ids
}
