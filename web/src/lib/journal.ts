import { decisionNote } from './rundecisions'
import { attemptUsageNote } from './runusage'
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
  /** A short readable detail for the few kinds that carry one worth a glance. */
  note?: string
}

// Pure function backing the run's full event journal view: every event the
// backend logs renders here, regardless of kind. Only the optional note
// branches on `type`, so an event kind this file has never heard of (a future
// reliability event, for instance) still renders its raw type/node instead of
// throwing or being silently dropped.
export function runEventJournal(events: WfEvent[]): EventJournalEntry[] {
  return events.map((e) => ({ seq: e.seq, type: e.type, node: e.node ?? null, note: eventNote(e) }))
}

// The detail line of an event kind that has one (issue #67): how a session
// handoff started, which declared output fields a node left out, where an
// attempt's transcript is, the tokens an attempt reported (issue #167),
// which working tree the run moved into, and what a decision model answered
// (issue #165). Every other kind has none.
function eventNote(e: WfEvent): string | undefined {
  const r = e as unknown as Record<string, unknown>
  switch (e.type) {
    case 'session_handoff':
      return r.warm
        ? `warm: continues the session of ${String(r.from_node ?? '')}`
        : `cold${r.reason ? ` (${String(r.reason)})` : ''}`
    case 'output_fields_missing':
      return Array.isArray(r.fields) ? `missing fields: ${r.fields.join(', ')}` : undefined
    case 'attempt_started':
      return typeof r.transcript === 'string' ? `transcript: ${r.transcript}` : undefined
    case 'attempt_finished':
      return attemptUsageNote(r.usage)
    case 'decision_made':
      return decisionNote(e)
    case 'worktree_resolved':
      return typeof r.path === 'string'
        ? `working tree: ${r.path} (${r.source === 'node' ? `published by ${String(r.node ?? '')}` : `from ${String(r.source ?? '')}`})`
        : undefined
    default:
      return undefined
  }
}
