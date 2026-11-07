import { describe, expect, it } from 'vitest'
import type { PreviousSuccessView } from './api.gen'
import { previousSuccessLine, untrustedLine } from './previoussuccess'

describe('previoussuccess', () => {
  it('names the earlier run and its times', () => {
    expect(
      previousSuccessLine({
        fallback: false,
        run_id: 'routine-1',
        started_at: '2026-11-06T07:00:00Z',
        finished_at: '2026-11-06T07:05:00Z',
      }),
    ).toBe('routine-1, started 2026-11-06T07:00:00Z, finished 2026-11-06T07:05:00Z')
  })

  it('names the when_none window and withheld keys', () => {
    expect(
      previousSuccessLine({ fallback: true, started_at: '2026-01-01T00:00:00Z' }),
    ).toBe('none (window from when_none: 2026-01-01T00:00:00Z)')
    expect(
      previousSuccessLine({ fallback: true, finished_at: '2026-01-02T00:00:00Z' }),
    ).toBe('none (window from when_none: finished 2026-01-02T00:00:00Z)')
    expect(
      previousSuccessLine({
        fallback: false,
        run_id: 'r1',
        started_at: 't',
        withheld: ['nodes.a.output.x'],
      }),
    ).toBe('r1, started t, withheld: nodes.a.output.x')
    expect(previousSuccessLine({ fallback: false })).toBe('none')
  })
})

describe('untrustedLine', () => {
  it('names the node, its sources and the accepted reason', () => {
    expect(
      untrustedLine({
        node: 'classify',
        sources: ['connector github on list_open'],
        accepted: 'labels come from params',
      }),
    ).toBe('node `classify` renders connector github on list_open; accepted: labels come from params')
    expect(
      untrustedLine({ node: 'w', sources: ['param body'] }),
    ).toBe('node `w` renders param body; not accepted')
  })
})
