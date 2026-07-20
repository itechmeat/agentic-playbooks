import type { WfEvent } from './types'

// An intervention journal entry: a supervisor wake-up or its action.
export interface JournalEntry {
  seq: number
  kind: 'wake' | 'action'
  label: string
  node?: string | null
  detail?: string
}

// Pure function: extracts only wake_raised/supervisor_action from the full
// list of run events, preserving the original order (by seq).
export function interventionJournal(events: WfEvent[]): JournalEntry[] {
  const entries: JournalEntry[] = []
  for (const e of events) {
    if (e.type === 'wake_raised') {
      entries.push({ seq: e.seq, kind: 'wake', label: e.trigger ?? 'wake', node: e.node, detail: e.detail })
    } else if (e.type === 'supervisor_action') {
      entries.push({ seq: e.seq, kind: 'action', label: e.action ?? 'action', node: e.node, detail: e.detail })
    }
  }
  return entries
}

// A single row in the full, chronological run event journal (the "events"
// tab in RunView.svelte).
export interface EventJournalEntry {
  seq: number
  type: string
  node?: string | null
}

// Pure function backing the run's full event journal view: every event the
// backend logs renders here, regardless of kind. No branching on `type`, so
// an event kind this file has never heard of (a future reliability event,
// for instance) still renders its raw type/node instead of throwing or
// being silently dropped.
export function runEventJournal(events: WfEvent[]): EventJournalEntry[] {
  return events.map((e) => ({ seq: e.seq, type: e.type, node: e.node ?? null }))
}
