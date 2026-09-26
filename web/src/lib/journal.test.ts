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
    ] as never)
    expect(entries.map((e) => e.note)).toEqual([
      'warm: continues the session of a',
      'cold (different model)',
      'missing fields: tree, verdict',
      'transcript: attempts/a-1',
      undefined,
    ])
  })
})
