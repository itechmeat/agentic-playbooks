import { describe, expect, it } from 'vitest'
import { interventionJournal, runEventJournal } from './journal'
import type { WfEvent } from './types'

const events: WfEvent[] = [
  { seq: 0, ts: 1, type: 'run_started' },
  { seq: 1, ts: 2, type: 'wake_raised', trigger: 'node_failed', node: 'a', detail: 'agent crashed' },
  { seq: 2, ts: 3, type: 'node_finished', node: 'a' },
  { seq: 3, ts: 4, type: 'supervisor_action', action: 'retry', node: 'a', detail: 'retried once' },
  { seq: 4, ts: 5, type: 'run_finished' },
]

describe('interventionJournal', () => {
  it('keeps only wake and action entries, in order', () => {
    const entries = interventionJournal(events)
    expect(entries).toHaveLength(2)
    expect(entries.map((e) => e.seq)).toEqual([1, 3])
  })

  it('maps wake_raised to kind wake with trigger as label', () => {
    const [wake] = interventionJournal(events)
    expect(wake).toMatchObject({ kind: 'wake', label: 'node_failed', node: 'a', detail: 'agent crashed' })
  })

  it('maps supervisor_action to kind action with action as label', () => {
    const [, action] = interventionJournal(events)
    expect(action).toMatchObject({ kind: 'action', label: 'retry', node: 'a', detail: 'retried once' })
  })
})

// Fixture journal exercising the run-reliability event kinds the dashboard
// had never seen before: run_resumed, edge_traversed, attempt_started with
// pid, and attempt_finished with duration_ms. Shapes mirror the exact serde
// tags/fields from crates/apb-engine/src/event.rs.
const reliabilityEvents: WfEvent[] = [
  { seq: 0, ts: 1, type: 'run_started', playbook: 'demo', version: '1' },
  { seq: 1, ts: 2, type: 'run_resumed', from_node: 'fix' },
  { seq: 2, ts: 3, type: 'edge_traversed', from: 'review', to: 'fix' },
  { seq: 3, ts: 4, type: 'attempt_started', node: 'fix', attempt: 1, agent: 'stub', pid: 4242 },
  { seq: 4, ts: 5, type: 'attempt_finished', node: 'fix', attempt: 1, status: 'succeeded', duration_ms: 1234 },
]

describe('runEventJournal', () => {
  it('renders every event generically without throwing, including new reliability event kinds', () => {
    expect(() => runEventJournal(reliabilityEvents)).not.toThrow()
    const entries = runEventJournal(reliabilityEvents)
    expect(entries).toHaveLength(reliabilityEvents.length)
    expect(entries.map((e) => e.type)).toEqual([
      'run_started',
      'run_resumed',
      'edge_traversed',
      'attempt_started',
      'attempt_finished',
    ])
    expect(entries[1]).toMatchObject({ seq: 1, type: 'run_resumed' })
    expect(entries[2]).toMatchObject({ seq: 2, type: 'edge_traversed' })
    expect(entries[3]).toMatchObject({ seq: 3, type: 'attempt_started', node: 'fix' })
    expect(entries[4]).toMatchObject({ seq: 4, type: 'attempt_finished', node: 'fix' })
  })
})

describe('runEventJournal notes', () => {
  it('says how a session handoff started and which declared fields were missing', () => {
    const entries = runEventJournal([
      { seq: 1, ts: 1, type: 'session_handoff', node: 'b', from_node: 'a', warm: true },
      { seq: 2, ts: 2, type: 'session_handoff', node: 'c', from_node: 'a', warm: false, reason: 'different model' },
      { seq: 3, ts: 3, type: 'output_fields_missing', node: 'a', fields: ['tree', 'verdict'] },
      { seq: 4, ts: 4, type: 'attempt_started', node: 'a', attempt: 1, transcript: 'attempts/a-1' },
      { seq: 5, ts: 5, type: 'node_started', node: 'a' },
      { seq: 6, ts: 6, type: 'worktree_resolved', path: '/src/wt', source: 'node', node: 'a' },
      { seq: 7, ts: 7, type: 'worktree_resolved', path: '/src/wt', source: 'caller' },
      { seq: 8, ts: 8, type: 'attempt_finished', node: 'a', usage: { input_tokens: 10, output_tokens: 2, source: 'reported' } },
      { seq: 9, ts: 9, type: 'attempt_finished', node: 'a' },
    ] as never)
    expect(entries.map((e) => e.note)).toEqual([
      'warm: continues the session of a',
      'cold (different model)',
      'missing fields: tree, verdict',
      'transcript: attempts/a-1',
      undefined,
      'working tree: /src/wt (published by a)',
      'working tree: /src/wt (from caller)',
      'tokens: 10 in, 2 out',
      undefined,
    ])
  })
})

describe('wake triage note', () => {
  const wake = { seq: 1, ts: 1, type: 'wake_raised', trigger: 'node_failed', node: 'a', detail: 'failed' }

  it('is hidden for a wake without triage', () => {
    expect(runEventJournal([wake as WfEvent])[0].note).toBeUndefined()
  })

  it('shows the triage action, p, looping and who answered', () => {
    const e = { ...wake, triage: { action: 'retry_with_note', p: 0.78, confidence: 0.6, looping_p: 0.1, provider: 'main', model: 'jev-1.13.0' } }
    expect(runEventJournal([e as unknown as WfEvent])[0].note).toBe(
      'triage: retry_with_note p=0.78, looping p=0.10, main/jev-1.13.0',
    )
  })

  it('marks a triage the engine applied', () => {
    const e = { ...wake, triage: { action: 'retry_same', p: 0.9, confidence: 0.8, provider: 'main', model: 'm', applied: true } }
    expect(runEventJournal([e as unknown as WfEvent])[0].note).toBe('triage: retry_same p=0.90, main/m (applied)')
  })
})

describe('host execution mode rows', () => {
  it('names the task, its hint, the submitter and a fallback reason', () => {
    const rows = runEventJournal([
      { seq: 1, ts: 1, type: 'host_task_requested', node: 'plan', task_id: 'plan-1', model_hint: 'sonnet' },
      { seq: 2, ts: 2, type: 'host_task_submitted', task_id: 'plan-1', status: 'succeeded', submitted_by: 'host', client: 'claude-code' },
      { seq: 3, ts: 3, type: 'host_task_submitted', task_id: 'build-1', status: 'expired', submitted_by: 'engine' },
      { seq: 4, ts: 4, type: 'execution_fallback', node: 'build', attempt: 2, reason: 'spawn `claude` failed' },
    ] as unknown as WfEvent[])
    expect(rows.map((r) => r.type)).toEqual([
      'host_task_requested',
      'host_task_submitted',
      'host_task_submitted',
      'execution_fallback',
    ])
    expect(rows[0]).toMatchObject({ node: 'plan', note: 'task plan-1, model hint sonnet' })
    expect(rows[1].note).toBe('task plan-1: succeeded by host (claude-code)')
    expect(rows[2].note).toBe('task build-1: expired by engine')
    expect(rows[3].note).toBe('no CLI could start, running as a host task: spawn `claude` failed')
  })
})
