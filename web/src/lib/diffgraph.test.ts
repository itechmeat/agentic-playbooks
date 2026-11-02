import { describe, expect, it } from 'vitest'
import type { VersionDiff } from './api.gen'
import { diffUnion } from './diffgraph'
import type { EdgeAddress } from './edgeaddress'

const d = (from: string, to: string, ordinal: number): EdgeAddress => ({ kind: 'derived', from, to, ordinal })
const pb = (nodes: string[], edges: Record<string, unknown>[]) =>
  ({
    id: 'p',
    name: 'p',
    nodes: nodes.map((id) => ({ id, type: id === 'start' ? 'start' : 'agent_task' })),
    edges,
  }) as never

const diff = (over: Partial<VersionDiff>): Pick<VersionDiff, 'nodes_added' | 'nodes_removed' | 'nodes_changed' | 'edges' | 'edge_order_changed'> => ({
  nodes_added: [],
  nodes_removed: [],
  nodes_changed: [],
  edges: [],
  edge_order_changed: [],
  ...over,
})

describe('diff union', () => {
  it('gives the deleted first duplicate and the kept one distinct canvas ids', () => {
    // Before: two id-less a -> b edges (#1, #2). After: the first is gone, so
    // the kept edge moved from #2 to #1.
    const before = pb(['start', 'a', 'b'], [
      { from: 'start', to: 'a' },
      { from: 'a', to: 'b', condition: { type: 'output_match', node: 'a', pattern: 'x' } },
      { from: 'a', to: 'b', fallback: true },
    ])
    const after = pb(['start', 'a', 'b'], [
      { from: 'start', to: 'a' },
      { from: 'a', to: 'b', fallback: true },
    ])
    const u = diffUnion(
      before,
      after,
      diff({
        edges: [
          { before: d('start', 'a', 1), after: d('start', 'a', 1), changed: [] },
          { before: d('a', 'b', 2), after: d('a', 'b', 1), changed: ['address'] },
          { before: d('a', 'b', 1), after: null, changed: [] },
        ],
      }),
    )
    expect(u.edges.map((e) => e.id)).toEqual(['c0', 'c1', 'before:2'])
    expect(new Set(u.edges.map((e) => e.id)).size).toBe(3)
    const kept = u.edges[1]
    expect(kept.data).toMatchObject({ diff: 'changed', before: 'e:a>b#2', after: 'e:a>b#1', tone: 'else' })
    expect(kept.label).toBe('changed: address')
    const ghost = u.edges[2]
    expect(ghost.data).toMatchObject({ diff: 'removed', before: 'e:a>b#1', tone: 'cond' })
    expect(ghost.data.after).toBeUndefined()
    expect(u.edges[0].data.diff).toBeUndefined()
  })

  it('marks added and removed nodes and edges, and a reordered source', () => {
    const before = pb(['start', 'old'], [{ from: 'start', to: 'old' }])
    const after = pb(['start', 'new'], [{ from: 'start', to: 'new' }])
    const u = diffUnion(
      before,
      after,
      diff({
        nodes_added: ['new'],
        nodes_removed: ['old'],
        nodes_changed: ['start'],
        edges: [
          { before: null, after: d('start', 'new', 1), changed: [] },
          { before: d('start', 'old', 1), after: null, changed: [] },
        ],
        edge_order_changed: [{ node: 'start', before: [], after: [] }],
      }),
    )
    expect(u.nodes.map((n) => [n.id, n.data.diff])).toEqual([
      ['start', 'changed'],
      ['new', 'added'],
      ['old', 'removed'],
    ])
    expect(u.nodes[0].data.orderChanged).toBe(true)
    expect(u.edges.map((e) => [e.id, e.data.diff])).toEqual([
      ['after:0', 'added'],
      ['before:1', 'removed'],
    ])
    // A diff reads the lines: no named exits, no handle ids.
    expect(u.edges.every((e) => e.sourceHandle === undefined)).toBe(true)
  })
})
