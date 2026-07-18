// Groups a reverse-chronological run list so each child run sits immediately
// after its parent (review R1-M4): newest-first order otherwise puts a child
// ABOVE its parent (it started later), which made the existing `pl-4` indent
// useless - there was nothing above it to indent under.
//
// Rule (kept intentionally simple):
//  - Top-level rows (no parent, or a parent not present in this same list -
//    an "orphan") keep their existing relative order.
//  - Each parent's children are inserted immediately after it, in their own
//    existing relative order.
//  - An orphan (parent_run set, but that parent_run is not the run_id of any
//    row in this list - e.g. paged out, or the parent lives on a different
//    page) is treated as top-level for ordering purposes: there is no parent
//    row in this list to anchor it under, so regrouping it would either drop
//    it or invent a position with no basis. It keeps its original slot.
//    RunList.svelte still renders its "child of <id>" caption from
//    `parent_run` directly, independent of this grouping.

export interface RunLike {
  run_id: string
  parent_run?: string | null
}

export function groupRunsByParent<T extends RunLike>(runs: T[]): T[] {
  const ids = new Set(runs.map((r) => r.run_id))
  const isGroupedChild = (r: T): boolean => !!r.parent_run && ids.has(r.parent_run)

  const childrenOf = new Map<string, T[]>()
  for (const r of runs) {
    if (!isGroupedChild(r)) continue
    const list = childrenOf.get(r.parent_run as string)
    if (list) list.push(r)
    else childrenOf.set(r.parent_run as string, [r])
  }

  const placed = new Set<string>()
  const out: T[] = []
  function place(r: T): void {
    if (placed.has(r.run_id)) return
    placed.add(r.run_id)
    out.push(r)
    for (const c of childrenOf.get(r.run_id) ?? []) place(c)
  }

  for (const r of runs) {
    if (isGroupedChild(r)) continue // placed via its parent, above
    place(r)
  }
  return out
}
