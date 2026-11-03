import { describe, expect, it } from 'vitest'
import { resultsKey } from './results'
import type { WfEvent } from './types'

describe('resultsKey', () => {
  const ev = (seq: number, type: string, extra: Record<string, unknown> = {}): WfEvent =>
    ({ seq, ts: seq, type, ...extra }) as WfEvent

  it('changes only when a node or a list item settles, so other journal notices fetch no results', () => {
    const start = [ev(1, 'run_started'), ev(2, 'node_started', { node: 'w' })]
    const k0 = resultsKey(start, 'running')
    // An attempt's output and progress settle nothing.
    expect(resultsKey([...start, ev(3, 'attempt_started', { node: 'w' }), ev(4, 'progress')], 'running')).toBe(k0)
    const settled = [...start, ev(5, 'node_finished', { node: 'w', status: 'succeeded' })]
    expect(resultsKey(settled, 'running')).not.toBe(k0)
    const item = [...settled, ev(6, 'list_item_settled', { node: 'l' })]
    expect(resultsKey(item, 'running')).not.toBe(resultsKey(settled, 'running'))
    // The run's end fetches once more.
    expect(resultsKey(item, 'succeeded')).not.toBe(resultsKey(item, 'running'))
  })
})
