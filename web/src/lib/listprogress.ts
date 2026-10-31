import type { RunList, RunListItem } from './api.gen'
import type { PlaybookNode } from './types'

// The run page's list panel and the graph's list badge: pure shaping of a
// list node's visit (`RunDetail.lists`). Counts come from the engine; these
// helpers only order, filter and word them.

// The item rows shown before "show all".
export const ITEM_ROW_CAP = 200

// The state filter of the item table. `with_note` is a passed item whose
// check accepted it with a note.
export type ItemFilter = 'all' | 'needs_look' | 'running' | 'pending' | 'passed' | 'with_note'

export const ITEM_FILTERS: readonly ItemFilter[] = ['all', 'needs_look', 'running', 'pending', 'passed', 'with_note']

// How a state (or a filter) reads, the one wording the chips, the filter
// and the item table share. `passed` is a pass without a note.
export const STATE_LABEL: Record<ItemFilter, string> = {
  all: 'all',
  needs_look: 'needs a look',
  running: 'running',
  pending: 'pending',
  passed: 'passed',
  with_note: 'with a note',
}

// An item's state in words (an unknown state reads as it is).
export function stateLabel(state: string): string {
  return (STATE_LABEL as Record<string, string>)[state] ?? state
}

export interface ListChip {
  key: Exclude<ItemFilter, 'all'>
  label: string
  count: number
}

// One chip per state, in the order a person reads them: what passed, what
// passed with a note, what needs a look, what is under way, what waits.
export function listChips(l: RunList): ListChip[] {
  return [
    { key: 'passed', label: STATE_LABEL.passed, count: l.passed - l.passed_with_note },
    { key: 'with_note', label: STATE_LABEL.with_note, count: l.passed_with_note },
    { key: 'needs_look', label: STATE_LABEL.needs_look, count: l.needs_look },
    { key: 'running', label: STATE_LABEL.running, count: l.running },
    { key: 'pending', label: STATE_LABEL.pending, count: l.pending },
  ]
}

// Processed share of the list, 0 to 100, rounded down: 100 only once
// every item is processed.
export function processedPercent(l: RunList): number {
  if (l.total <= 0) return 0
  return Math.min(100, Math.floor((l.processed / l.total) * 100))
}

// Pending items with no row yet: an item not admitted to a batch has none,
// though `pending` counts it.
export function unlistedPending(l: RunList): number {
  const listed = l.items.filter((i) => i.state === 'pending').length
  return Math.max(0, l.pending - listed)
}

// `batch 2 of 5`, or null before the first batch.
export function batchLabel(l: RunList): string | null {
  return l.batch > 0 ? `batch ${l.batch} of ${Math.max(l.batches, l.batch)}` : null
}

// `12 of 43 processed`.
export function processedLabel(l: RunList): string {
  return `${l.processed} of ${l.total} processed`
}

function withNote(i: RunListItem): boolean {
  return i.state === 'passed' && i.verdict === 'accept_with_note'
}

// `passed` and `with_note` split the passed items as their chips count
// them.
export function matchesFilter(i: RunListItem, f: ItemFilter): boolean {
  if (f === 'all') return true
  if (f === 'with_note') return withNote(i)
  if (f === 'passed') return i.state === 'passed' && !withNote(i)
  return i.state === f
}

// What a person acts on first: items that need a look, then running ones,
// pending ones and passed ones; by id within a state.
const STATE_ORDER: Record<string, number> = { needs_look: 0, running: 1, pending: 2, passed: 3 }

export function sortItems(items: readonly RunListItem[]): RunListItem[] {
  return [...items].sort(
    (a, b) => (STATE_ORDER[a.state] ?? 9) - (STATE_ORDER[b.state] ?? 9) || a.id.localeCompare(b.id),
  )
}

// The rows the table shows under a filter, capped unless `showAll`, and how
// many matching rows the cap hides.
export function visibleItems(
  items: readonly RunListItem[],
  f: ItemFilter,
  showAll: boolean,
): { rows: RunListItem[]; hidden: number } {
  const matching = sortItems(items.filter((i) => matchesFilter(i, f)))
  if (showAll || matching.length <= ITEM_ROW_CAP) return { rows: matching, hidden: 0 }
  return { rows: matching.slice(0, ITEM_ROW_CAP), hidden: matching.length - ITEM_ROW_CAP }
}

// What an item row says beside its state: the reason it needs a look, the
// check's note, or nothing.
export function itemText(i: RunListItem): string {
  const reason = i.reason ? i.reason.replace(/_/g, ' ') : null
  return [reason, i.note].filter((s): s is string => !!s).join(': ')
}

// The graph badge of a list node: `12/43` once its visit started, `list`
// before; null for a node without a `list:` block.
export function listBadge(node: PlaybookNode, progress?: { processed: number; total: number }): string | null {
  if (!node.list) return null
  return progress ? `${progress.processed}/${progress.total}` : 'list'
}

// Each list's progress by node id, for `listBadge`.
export function listProgressByNode(lists: readonly RunList[] | undefined): Record<string, { processed: number; total: number }> {
  return Object.fromEntries((lists ?? []).map((l) => [l.node, { processed: l.processed, total: l.total }]))
}
