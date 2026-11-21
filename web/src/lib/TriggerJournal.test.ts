import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import TriggerJournal from './TriggerJournal.svelte'
import type { LogEntry } from './api.gen'

describe('TriggerJournal', () => {
  it('renders a rule detail journal whose lines lack the optional arrays', () => {
    // The wire shape of a swept rule's journal: a `positioned` line carries
    // no `codes` and no `conditions` (they are left out when empty).
    const journal = [
      { id: 'inc:positioned:4', at: 1700000000000, verdict: 'positioned', range: [4, 4] },
      { id: 'inc:5:started', at: 1700000001000, verdict: 'started', seq: 5, run_id: 'run-5' },
    ] as LogEntry[]
    const { body } = render(TriggerJournal, { props: { journal } })
    expect(body).toContain('positioned at the current head of the source')
    expect(body).toContain('event 5: started a run; run run-5')
  })

  it('renders nothing for an empty journal', () => {
    const { body } = render(TriggerJournal, { props: { journal: [] } })
    expect(body).not.toContain('trigger-journal')
  })
})
