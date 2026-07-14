import { describe, expect, it } from 'vitest'
import { interventionJournal } from './journal'
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
