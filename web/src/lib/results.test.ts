import { describe, expect, it } from 'vitest'
import { resultsFetch, resultsKey } from './results'
import type { WfEvent } from './types'

describe('resultsKey', () => {
  const ev = (seq: number, type: string, extra: Record<string, unknown> = {}): WfEvent =>
    ({ seq, ts: seq, type, ...extra }) as WfEvent

  it('changes at a settlement or an attempt boundary, so other journal notices fetch no results', () => {
    const start = [ev(1, 'run_started'), ev(2, 'node_started', { node: 'w' })]
    const k0 = resultsKey(start, 'running')
    // Progress and output notices change nothing.
    expect(resultsKey([...start, ev(3, 'progress'), ev(4, 'node_output', { node: 'w' })], 'running')).toBe(k0)
    // An attempt's start opens its evidence session: the session shows
    // while the attempt runs.
    const opened = [...start, ev(3, 'attempt_started', { node: 'w' })]
    expect(resultsKey(opened, 'running')).not.toBe(k0)
    // Its end closes the session: its final reads and its claims are
    // fetched without waiting for the node to settle.
    const closed = [...opened, ev(4, 'attempt_finished', { node: 'w' })]
    expect(resultsKey(closed, 'running')).not.toBe(resultsKey(opened, 'running'))
    const settled = [...closed, ev(5, 'node_finished', { node: 'w', status: 'succeeded' })]
    expect(resultsKey(settled, 'running')).not.toBe(resultsKey(closed, 'running'))
    const item = [...settled, ev(6, 'list_item_settled', { node: 'l' })]
    expect(resultsKey(item, 'running')).not.toBe(resultsKey(settled, 'running'))
    // The run's end fetches once more.
    expect(resultsKey(item, 'succeeded')).not.toBe(resultsKey(item, 'running'))
  })
})

describe('resultsFetch', () => {
  it('fetches a key once, and again after a current failure cleared what was shown', () => {
    const f = resultsFetch()
    expect(f.needed('1:0:running')).toBe(true)
    const t = f.begin()
    expect(f.succeeded(t, '1:0:running')).toBe(true)
    expect(f.needed('1:0:running')).toBe(false)
    const t2 = f.begin()
    expect(f.failed(t2)).toBe(true)
    expect(f.needed('1:0:running')).toBe(true)
  })

  it('drops a superseded answer, so an older success never marks a key a newer failure cleared', () => {
    const f = resultsFetch()
    const first = f.begin()
    const second = f.begin()
    expect(f.succeeded(first, 'k')).toBe(false)
    expect(f.failed(second)).toBe(true)
    expect(f.needed('k')).toBe(true)
    // And an older failure never clears what a newer success showed.
    const a = f.begin()
    const b = f.begin()
    expect(f.succeeded(b, 'k')).toBe(true)
    expect(f.failed(a)).toBe(false)
    expect(f.needed('k')).toBe(false)
    // A route change forgets the key and every answer still on its way.
    const c = f.begin()
    f.reset()
    expect(f.succeeded(c, 'k')).toBe(false)
    expect(f.needed('k')).toBe(true)
  })
})
