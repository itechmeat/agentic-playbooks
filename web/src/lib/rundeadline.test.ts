import { describe, expect, it } from 'vitest'
import { deadlineLine, durationText } from './rundeadline'
import { runEventJournal } from './journal'
import type { WfEvent } from './types'

describe('rundeadline', () => {
  const base = { total_ms: 45 * 60_000, elapsed_ms: 0, source: 'playbook' as const }

  it('writes durations the way an author does', () => {
    expect(durationText(45 * 60_000)).toBe('45m')
    expect(durationText(90 * 60_000)).toBe('1h30m')
    expect(durationText(500)).toBe('0s')
  })

  it('has the three line forms', () => {
    expect(
      deadlineLine({ ...base, remaining_ms: 12 * 60_000, frozen: false, ends_at: '2026-11-10T10:00:00Z' }),
    ).toBe('Deadline 45m, 12m left (ends 2026-11-10T10:00:00Z)')
    expect(deadlineLine({ ...base, remaining_ms: 12 * 60_000, frozen: true })).toBe(
      'Deadline 45m, 12m left, frozen (paused by a person)',
    )
    expect(deadlineLine({ ...base, remaining_ms: 0, frozen: true })).toBe('Deadline 45m, exceeded')
  })

  it('reads a deadline abort in the journal', () => {
    const rows = runEventJournal([
      { seq: 1, type: 'run_aborted', reason: 'deadline_exceeded: the run passed its deadline of 45m' },
      { seq: 2, type: 'run_aborted', reason: 'stop requested' },
    ] as unknown as WfEvent[])
    expect(rows[0].note).toBe('deadline exceeded')
    expect(rows[1].note).toBeUndefined()
  })
})
