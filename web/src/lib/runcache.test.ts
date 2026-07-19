import { describe, it, expect } from 'vitest'
import { cachedNodeIds } from './runcache'
import type { WfEvent } from './types'

// Real serde shape (crates/apb-engine/src/event.rs EventPayload, #[serde(tag
// = "type", rename_all = "snake_case")]): flat, e.g.
// { type: "node_cache_hit", node: "b", key: "sha256:x", source_run: "r0" }.
const ev = (seq: number, type: string, extra: Record<string, unknown> = {}): WfEvent =>
  ({ seq, ts: 0, type, ...extra }) as WfEvent

describe('cachedNodeIds', () => {
  it('collects nodes with a cache hit', () => {
    const events: WfEvent[] = [
      ev(0, 'node_started', { node: 'a', attempt: 1 }),
      ev(1, 'node_cache_hit', { node: 'b', key: 'sha256:x', source_run: 'r0' }),
      ev(2, 'node_cache_miss', { node: 'c', key: 'sha256:y' }),
    ]
    expect(cachedNodeIds(events)).toEqual(new Set(['b']))
  })

  it('empty on no events', () => {
    expect(cachedNodeIds([])).toEqual(new Set())
  })

  it('collects multiple cache hits and ignores stored/rejected events', () => {
    const events: WfEvent[] = [
      ev(0, 'node_cache_hit', { node: 'a', key: 'sha256:x', source_run: 'r0' }),
      ev(1, 'node_cache_hit', { node: 'b', key: 'sha256:y', source_run: 'r0' }),
      ev(2, 'node_cache_stored', { node: 'c', key: 'sha256:z' }),
      ev(3, 'node_cache_rejected', { node: 'd', reason: 'too_large' }),
    ]
    expect(cachedNodeIds(events)).toEqual(new Set(['a', 'b']))
  })

  it('ignores a cache hit event with no node field', () => {
    expect(cachedNodeIds([ev(0, 'node_cache_hit')])).toEqual(new Set())
  })
})
