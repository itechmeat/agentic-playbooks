import { decisionNote } from './rundecisions'
import { outcomeEventNote } from './runoutcome'
import { attemptOutputNote } from './outputloss'
import { attemptUsageNote } from './runusage'
import { notifyEventNote, notifySetNote } from './notify'
import type { WfEvent } from './types'

// An intervention journal entry: a supervisor wake-up or its action.
export interface JournalEntry {
  seq: number
  kind: 'wake' | 'action'
  label: string
  node?: string | null
  detail?: string
}

// A park wake's decision-model triage (issue #165 Part 10), as one line:
// `triage: retry_with_note p=0.78, looping p=0.10, provider/model`. Nothing for
// a wake without one (the use off, or the provider down).
export function triageNote(t: unknown): string | undefined {
  if (!t || typeof t !== 'object') return undefined
  const r = t as Record<string, unknown>
  if (typeof r.action !== 'string' || !r.action) return undefined
  const p = typeof r.p === 'number' ? ` p=${r.p.toFixed(2)}` : ''
  const looping = typeof r.looping_p === 'number' ? `, looping p=${r.looping_p.toFixed(2)}` : ''
  const who = [r.provider, r.model].filter((x) => typeof x === 'string' && x).join('/')
  return `triage: ${r.action}${p}${looping}${who ? `, ${who}` : ''}${r.applied ? ' (applied)' : ''}`
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

// A profile's `process_env: allowlist` on one attempt: how many operator
// variables the agent kept, which it did not get (the first few by name)
// and which allowed names the operator does not set. Names only.
export function agentEnvNote(r: Record<string, unknown>): string {
  const withheld = Array.isArray(r.withheld) ? r.withheld.map(String) : []
  const more = typeof r.withheld_more === 'number' ? r.withheld_more : 0
  const total = withheld.length + more
  const shown = withheld.slice(0, 5)
  const names = shown.length ? ` (${shown.join(', ')}${total > shown.length ? ', ...' : ''})` : ''
  const absent = Array.isArray(r.absent) && r.absent.length ? `, not set: ${listOf(r.absent)}` : ''
  const agent = typeof r.agent === 'string' && r.agent ? ` (${r.agent})` : ''
  return `env allowlist${agent}: kept ${String(r.kept ?? 0)}, withheld ${total}${names}${absent}`
}

/** A list of node ids as `a, b`, for a note line. */
function listOf(v: unknown): string {
  return Array.isArray(v) ? v.map(String).join(', ') : ''
}

// The detail line of an event kind that has one (issue #67): how a session
// handoff started, which declared output fields a node left out, where an
// attempt's transcript is, the tokens an attempt reported (issue #167) and
// what it lost of its output, which working tree the run moved into, what a decision model answered
// (issue #165), and what a fork's branch-failure policy or a refusing join did
// (issue #195), or which inputs a join wrote off and a re-run revived
// (issue #203), and what a re-run revives, the baseline check's snapshots
// and comparisons, the notes an attempt reported, and what a process_env
// allowlist withheld from an attempt, and the run's notifications (who
// changed them, and each notification sent). Every other kind has none.
function eventNote(e: WfEvent): string | undefined {
  const r = e as unknown as Record<string, unknown>
  // A notification's send rides the send ledger and the connector call
  // events under the engine site `@notify`.
  const notification = notifyEventNote(e.type, r)
  if (notification) return notification
  switch (e.type) {
    case 'session_handoff':
      return r.warm
        ? `warm: continues the session of ${String(r.from_node ?? '')}`
        : `cold${r.reason ? ` (${String(r.reason)})` : ''}`
    // 0.23.0 outcome events (provenance, goal criteria, protected paths).
    case 'artifacts_committed':
    case 'goal_checked':
    case 'protected_paths_modified':
    case 'inbox_gap':
    case 'baseline_observed':
    case 'baseline_compared':
    case 'attempt_notes':
      return outcomeEventNote(e)
    case 'output_fields_missing':
      return Array.isArray(r.fields) ? `missing fields: ${r.fields.join(', ')}` : undefined
    case 'attempt_started':
      return typeof r.transcript === 'string' ? `transcript: ${r.transcript}` : undefined
    case 'agent_env_applied':
      return agentEnvNote(r)
    case 'attempt_finished': {
      const notes = [attemptUsageNote(r.usage), attemptOutputNote(r.output_truncated, r.transcript_incomplete)]
      const line = notes.filter(Boolean).join(' · ')
      return line || undefined
    }
    case 'decision_made':
      return decisionNote(e)
    case 'wake_raised':
      return triageNote(r.triage)
    // Host execution mode (0.23.0): the request and submission rows.
    // `hint_note` is the engine's own label of the model hint; an event
    // from before it existed gets a short generic one.
    case 'host_task_requested': {
      const hint =
        typeof r.hint_note === 'string' && r.hint_note
          ? r.hint_note
          : r.model_hint
            ? `advisory: model hint ${String(r.model_hint)} (declared by the profile)`
            : ''
      return `task ${String(r.task_id ?? '')}${hint ? `, ${hint}` : ''}`
    }
    case 'host_task_submitted':
      return `task ${String(r.task_id ?? '')}: ${String(r.status ?? '')} by ${String(r.submitted_by ?? '')}${r.client ? ` (${String(r.client)})` : ''}${r.model ? `, ran on ${String(r.model)} (reported)` : ''}`
    case 'execution_fallback':
      return `no CLI could start, running as a host task${r.reason ? `: ${String(r.reason)}` : ''}`
    // Issue #195: fork branch-failure policies and refused joins.
    case 'branch_failed':
      return r.target
        ? `fork ${String(r.fork ?? '')} ${String(r.policy ?? '')}: failed, the run goes to ${String(r.target)}`
        : `fork ${String(r.fork ?? '')} ${String(r.policy ?? '')}: failed, the other branches are cancelled`
    case 'branch_cancelled':
      return `fork ${String(r.fork ?? '')}: cancelled after ${String(r.failed_node ?? '')} failed`
    case 'join_refused':
      return typeof r.reason === 'string' && r.reason ? r.reason : undefined
    // Issue #203: a join written off an input, and a re-run that revived it.
    case 'join_input_dead': {
      const list = listOf(r.sources)
      return r.node && list ? `${String(r.node)} proceeds without ${list}` : undefined
    }
    case 'join_input_revived': {
      if (!r.node) return undefined
      const list = listOf(r.sources)
      const from = r.from_node ? ` (re-run of ${String(r.from_node)})` : ''
      return `${String(r.node)} waits for ${list || 'its revived inputs'} again${from}`
    }
    case 'nodes_revived': {
      const list = listOf(r.nodes)
      if (!list) return undefined
      return r.from_node ? `re-run of ${String(r.from_node)} revives ${list}` : `a re-run revives ${list}`
    }
    case 'notify_set':
      return notifySetNote(r)
    case 'worktree_resolved':
      return typeof r.path === 'string'
        ? `working tree: ${r.path} (${r.source === 'node' ? `published by ${String(r.node ?? '')}` : `from ${String(r.source ?? '')}`})`
        : undefined
    default:
      return undefined
  }
}
