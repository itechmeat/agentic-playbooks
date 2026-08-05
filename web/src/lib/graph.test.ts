import { describe, expect, it } from 'vitest'
import { failureEffect, isUserArranged, nodeExits, toFlow } from './graph'

const playbook = {
  id: 'demo',
  name: 'Demo',
  nodes: [
    { id: 'start', type: 'start', title: 'Start' },
    { id: 'a', type: 'agent_task', title: 'A' },
    { id: 'done', type: 'finish', title: null },
  ],
  edges: [
    { from: 'start', to: 'a' },
    { from: 'a', to: 'done', condition: { type: 'node_status', node: 'a', equals: 'success' } },
  ],
}

describe('toFlow', () => {
  it('maps nodes and edges', () => {
    const { nodes, edges } = toFlow(playbook, null)
    expect(nodes).toHaveLength(3)
    expect(nodes[0]).toMatchObject({ id: 'start', type: 'playbookNode', data: { kind: 'start', title: 'Start' } })
    // a node without a title gets its id as the title
    expect(nodes[2].data.title).toBe('done')
    expect(edges).toHaveLength(2)
    expect(edges[1]).toMatchObject({ source: 'a', target: 'done' })
    expect(edges[1].label).toContain('success')
  })

  it('uses stored layout positions when present', () => {
    const layout = { nodes: [{ id: 'a', x: 111, y: 222 }] }
    const { nodes } = toFlow(playbook, layout)
    const a = nodes.find((n) => n.id === 'a')!
    expect(a.position).toEqual({ x: 111, y: 222 })
  })

  it('auto-layouts nodes top-to-bottom without stored positions', () => {
    const { nodes } = toFlow(playbook, null)
    const ys = nodes.map((n) => n.position.y)
    // dagre lays out ranks vertically: start above a, a above done
    expect(ys[0]).toBeLessThan(ys[1])
    expect(ys[1]).toBeLessThan(ys[2])
  })

  it('annotates nodes with run status when provided', () => {
    const statuses = { start: 'succeeded', a: 'running', done: 'pending' }
    const { nodes } = toFlow(playbook, null, statuses)
    expect(nodes.find((n) => n.id === 'a')!.data.status).toBe('running')
    expect(nodes.find((n) => n.id === 'start')!.data.status).toBe('succeeded')
  })

  it('leaves status undefined when no statuses given', () => {
    const { nodes } = toFlow(playbook, null)
    expect(nodes[0].data.status).toBeUndefined()
  })

  it('flags nodes present in cachedIds', () => {
    const { nodes } = toFlow(playbook, null, undefined, new Set(['a']))
    expect(nodes.find((n) => n.id === 'a')!.data.cached).toBe(true)
    expect(nodes.find((n) => n.id === 'start')!.data.cached).toBe(false)
  })

  it('leaves cached undefined when no cachedIds given', () => {
    const { nodes } = toFlow(playbook, null)
    expect(nodes[0].data.cached).toBeUndefined()
  })
})

// review -> fix -> review is a bounded loop (max_traversals), legal since the
// run-reliability change. Playbook graphs are no longer guaranteed to be
// DAGs, so layout must not throw and must not hang.
const cyclicPlaybook = {
  id: 'loopy',
  name: 'Loopy',
  nodes: [
    { id: 'review', type: 'agent_task', title: 'Review' },
    { id: 'fix', type: 'agent_task', title: 'Fix' },
    { id: 'done', type: 'finish', title: 'Done' },
  ],
  edges: [
    { from: 'review', to: 'fix', condition: { type: 'node_status', node: 'review', equals: 'failed' } },
    { from: 'fix', to: 'review', max_traversals: 2 },
    { from: 'review', to: 'done', condition: { type: 'node_status', node: 'review', equals: 'success' } },
  ],
}

describe('toFlow with a cyclic (bounded-loop) playbook', () => {
  it('lays out the cycle without throwing and renders all three nodes', () => {
    expect(() => toFlow(cyclicPlaybook, null)).not.toThrow()
    const { nodes, edges } = toFlow(cyclicPlaybook, null)
    expect(nodes).toHaveLength(3)
    expect(nodes.map((n) => n.id).sort()).toEqual(['done', 'fix', 'review'])
    expect(edges).toHaveLength(3)
  })
})

// Spec 2026-07-26: with `defaults.on_failure: stop`, a node whose failure has
// nowhere to go ends the run, and the canvas has to say so. The predicate is
// deliberately narrow - anything that might still route a failure is left
// unmarked rather than guessed at.
function pbWith(
  edges: { from: string; to: string; condition?: unknown; fallback?: boolean }[],
  onFailure = 'stop',
) {
  return {
    id: 'p',
    name: 'P',
    defaults: { on_failure: onFailure },
    nodes: [
      { id: 'work', type: 'agent_task', title: 'Work' },
      { id: 'other', type: 'agent_task', title: 'Other' },
      { id: 'done', type: 'finish', title: 'Done' },
    ],
    edges,
  } as Parameters<typeof failureEffect>[0]
}

const successEdge = {
  from: 'work',
  to: 'done',
  condition: { type: 'node_status', node: 'work', equals: 'success' },
}

describe('failureEffect', () => {
  it('marks a node whose only edge is its own success', () => {
    expect(failureEffect(pbWith([successEdge]), 'work')).toEqual({ kind: 'stop' })
  })

  it('names the handler when the policy routes to a node', () => {
    expect(failureEffect(pbWith([successEdge], 'other'), 'work')).toEqual({
      kind: 'route',
      node: 'other',
    })
  })

  it('claims nothing on the handler itself', () => {
    const pb = pbWith([successEdge, { ...successEdge, from: 'other' }], 'other')
    expect(failureEffect(pb, 'other')).toBeNull()
  })

  it('does not mark anything while the policy is route', () => {
    expect(failureEffect(pbWith([successEdge], 'route'), 'work')).toBeNull()
  })

  it('does not mark a node whose failure has an edge', () => {
    const failureEdge = {
      from: 'work',
      to: 'done',
      condition: { type: 'node_status', node: 'work', equals: 'failure' },
    }
    expect(failureEffect(pbWith([successEdge, failureEdge]), 'work')).toBeNull()
  })

  it.each([
    ['an unconditional edge', { from: 'work', to: 'done' }],
    ['a fallback edge', { from: 'work', to: 'done', fallback: true }],
    [
      'an output_match edge that may match a failed output',
      { from: 'work', to: 'done', condition: { type: 'output_match', node: 'work', pattern: 'x' } },
    ],
    [
      "a condition on another node's status",
      {
        from: 'work',
        to: 'done',
        condition: { type: 'node_status', node: 'other', equals: 'success' },
      },
    ],
  ])('leaves the node unmarked when it also has %s', (_label, edge) => {
    expect(failureEffect(pbWith([successEdge, edge]), 'work')).toBeNull()
  })

  it('leaves a node with no outgoing edge at all unmarked', () => {
    expect(failureEffect(pbWith([]), 'work')).toBeNull()
  })

  it('leaves a non-executing kind unmarked', () => {
    const pb = pbWith([{ ...successEdge, from: 'done' }])
    expect(failureEffect(pb, 'done')).toBeNull()
  })

  it('reaches the flow nodes', () => {
    const { nodes } = toFlow(pbWith([successEdge]), null)
    expect(nodes.find((n) => n.id === 'work')?.data.failure).toEqual({ kind: 'stop' })
    expect(nodes.find((n) => n.id === 'done')?.data.failure).toBeNull()
  })
})

// Review follow-ups on the marker predicate.
describe('failureEffect edge cases', () => {
  it('marks a wait node, which fails when its own timeout elapses', () => {
    const pb = {
      id: 'p',
      name: 'P',
      defaults: { on_failure: 'stop' },
      nodes: [
        { id: 'hold', type: 'wait', title: 'Hold' },
        { id: 'done', type: 'finish', title: 'Done' },
      ],
      edges: [
        {
          from: 'hold',
          to: 'done',
          condition: { type: 'node_status', node: 'hold', equals: 'success' },
        },
      ],
    } as Parameters<typeof failureEffect>[0]
    expect(failureEffect(pb, 'hold')).toEqual({ kind: 'stop' })
  })

  it('still stops a node that happens to be named stop', () => {
    const pb = {
      id: 'p',
      name: 'P',
      defaults: { on_failure: 'stop' },
      nodes: [
        { id: 'stop', type: 'agent_task', title: 'Stop' },
        { id: 'done', type: 'finish', title: 'Done' },
      ],
      edges: [
        {
          from: 'stop',
          to: 'done',
          condition: { type: 'node_status', node: 'stop', equals: 'success' },
        },
      ],
    } as Parameters<typeof failureEffect>[0]
    expect(failureEffect(pb, 'stop')).toEqual({ kind: 'stop' })
  })

  it('names no handler that does not exist', () => {
    expect(failureEffect(pbWith([successEdge], 'ghost'), 'work')).toBeNull()
  })
})

// Several edges leaving one point hid two opposite meanings and the order that
// decides between them. `nodeExits` is what makes both visible.
describe('nodeExits', () => {
  function pb(edges: unknown[], nodes = ['work', 'a', 'b', 'c']) {
    return {
      id: 'p',
      name: 'P',
      nodes: nodes.map((id) => ({ id, type: 'agent_task', title: id })),
      edges,
    } as Parameters<typeof nodeExits>[0]
  }
  const status = (equals: string, to: string, node = 'work') => ({
    from: 'work',
    to,
    condition: { type: 'node_status', node, equals },
  })

  it('says nothing about a node with one exit', () => {
    expect(nodeExits(pb([status('success', 'a')]), 'work')).toBeNull()
    expect(nodeExits(pb([]), 'work')).toBeNull()
  })

  it('numbers conditional exits in the order the engine checks them', () => {
    const exits = nodeExits(pb([status('success', 'a'), status('failure', 'b')]), 'work')
    expect(exits?.mode).toBe('one-of')
    expect(exits?.list.map((e) => e.label)).toEqual(['1 success', '2 failure'])
    expect(exits?.list.map((e) => e.id)).toEqual(['out-0', 'out-1'])
    expect(exits?.list.map((e) => e.tone)).toEqual(['success', 'failure'])
    expect(exits?.list[1].title).toContain('checked 2 of 2 in order')
    expect(exits?.list[1].title).toContain('goes to b')
  })

  it('calls an unconditional fan-out what it is, with no ordering implied', () => {
    const exits = nodeExits(pb([{ from: 'work', to: 'a' }, { from: 'work', to: 'b' }]), 'work')
    expect(exits?.mode).toBe('all')
    expect(exits?.list.map((e) => e.label)).toEqual(['a', 'b'])
    expect(exits?.list[0].title).toContain('in parallel')
  })

  it('names a fallback `else` and a foreign status by its node', () => {
    const exits = nodeExits(
      pb([status('success', 'a'), status('failure', 'b', 'lint'), { from: 'work', to: 'c', fallback: true }]),
      'work',
    )
    expect(exits?.list.map((e) => e.label)).toEqual(['1 success', '2 lint: failure', '3 else'])
  })

  // The real case that exposed this: both of pick_task's patterns start with
  // `needs_brainstorm: `, so an end-truncated caption read the same on both.
  it('keeps what tells two output_match exits apart', () => {
    const exits = nodeExits(
      pb([
        { from: 'work', to: 'a', condition: { type: 'output_match', node: 'work', pattern: 'needs_brainstorm: yes' } },
        { from: 'work', to: 'b', condition: { type: 'output_match', node: 'work', pattern: 'needs_brainstorm: no' } },
      ]),
      'work',
    )
    expect(exits?.list.map((e) => e.label)).toEqual(['1 match: yes', '2 match: no'])
    expect(exits?.list[0].title).toContain('needs_brainstorm: yes')
  })

  // Without its own arm an `output_field` exit fell through to the raw type
  // name, so both branches of a status-file verdict read `output_field`.
  it('names an output_field exit by the field and the value it routes on', () => {
    const exits = nodeExits(
      pb([
        { from: 'work', to: 'a', condition: { type: 'output_field', node: 'work', field: 'verdict', equals: 'failed' } },
        { from: 'work', to: 'b', condition: { type: 'output_field', node: 'lint', field: 'verdict', equals: 'ok' } },
      ]),
      'work',
    )
    expect(exits?.list.map((e) => e.label)).toEqual(['1 verdic…failed', '2 verdict: ok'])
    expect(exits?.list[0].title).toContain(`this node's output is a JSON object whose "verdict" equals "failed"`)
    expect(exits?.list[1].title).toContain('node lint')
  })

  it('truncates in the middle, so a shared prefix does not collapse two exits', () => {
    const exits = nodeExits(
      pb(
        [
          { from: 'work', to: 'lock_scope_alpha' },
          { from: 'work', to: 'lock_scope_omega' },
        ],
        ['work', 'lock_scope_alpha', 'lock_scope_omega'],
      ),
      'work',
    )
    const labels = exits?.list.map((e) => e.label) ?? []
    expect(labels[0]).not.toBe(labels[1])
    expect(labels[0]).toContain('…')
    expect(exits?.list[0].title).toContain('lock_scope_alpha')
  })

  it('gives every edge its own handle and drops the now-duplicated edge label', () => {
    const model = pb([status('success', 'a'), status('failure', 'b')])
    const { nodes, edges } = toFlow(model, null)
    expect(nodes.find((n) => n.id === 'work')?.data.exits?.list).toHaveLength(2)
    expect(edges.map((e) => e.sourceHandle)).toEqual(['out-0', 'out-1'])
    expect(edges.map((e) => e.label)).toEqual([undefined, undefined])
  })

  it('leaves a single-exit node on the anonymous handle with its edge label', () => {
    const { edges } = toFlow(pb([status('success', 'a')]), null)
    expect(edges[0].sourceHandle).toBeUndefined()
    expect(edges[0].label).toBe('work: success')
  })
})

// A fan-out from one source to several distinct successors has to read in the
// order the source's exits are declared. dagre on its own (issue #68 item 2)
// was reversing siblings whose barycenters tied, so the branch arrows crossed;
// the algorithm now feeds dagre order constraints so each source's distinct
// successors keep their declaration order, left to right.
const fanOutPlaybook = {
  id: 'fanout',
  name: 'Fanout',
  nodes: [
    { id: 'start', type: 'start', title: 'Start' },
    { id: 'a', type: 'agent_task', title: 'A' },
    { id: 'b', type: 'agent_task', title: 'B' },
    { id: 'c', type: 'agent_task', title: 'C' },
    { id: 'done', type: 'finish', title: 'Done' },
  ],
  edges: [
    { from: 'start', to: 'a' },
    { from: 'start', to: 'b' },
    { from: 'start', to: 'c' },
    { from: 'a', to: 'done' },
    { from: 'b', to: 'done' },
    { from: 'c', to: 'done' },
  ],
} as Parameters<typeof toFlow>[0]

describe('toFlow fan-out ordering', () => {
  it('places distinct successors left-to-right in declaration order', () => {
    const { nodes } = toFlow(fanOutPlaybook, null)
    const x = (id: string) => nodes.find((n) => n.id === id)!.position.x
    // Compare relative order, not absolute pixel values: dagre's spacing is
    // its own, what matters is that A is left of B is left of C.
    expect(x('a')).toBeLessThan(x('b'))
    expect(x('b')).toBeLessThan(x('c'))
  })

  it('keeps the order when successors are conditional (one-of) exits', () => {
    const pb = {
      id: 'cond',
      name: 'Cond',
      nodes: [
        { id: 'start', type: 'start', title: 'Start' },
        { id: 'work', type: 'agent_task', title: 'Work' },
        { id: 'a', type: 'agent_task', title: 'A' },
        { id: 'b', type: 'agent_task', title: 'B' },
        { id: 'c', type: 'agent_task', title: 'C' },
        { id: 'done', type: 'finish', title: 'Done' },
      ],
      edges: [
        { from: 'start', to: 'work' },
        { from: 'work', to: 'a', condition: { type: 'node_status', node: 'work', equals: 'success' } },
        { from: 'work', to: 'b', condition: { type: 'node_status', node: 'work', equals: 'failure' } },
        { from: 'work', to: 'c', fallback: true },
        { from: 'a', to: 'done' },
        { from: 'b', to: 'done' },
        { from: 'c', to: 'done' },
      ],
    } as Parameters<typeof toFlow>[0]
    const { nodes } = toFlow(pb, null)
    const x = (id: string) => nodes.find((n) => n.id === id)!.position.x
    expect(x('a')).toBeLessThan(x('b'))
    expect(x('b')).toBeLessThan(x('c'))
  })
})

// A layout may be marked `userArranged: true` once the user has dragged nodes
// by hand. The marker is additive: absent means "not user-arranged" and keeps
// every existing call site unchanged.
describe('isUserArranged', () => {
  it('returns false for null', () => {
    expect(isUserArranged(null)).toBe(false)
  })
  it('returns false for a layout without the marker', () => {
    expect(isUserArranged({ nodes: [{ id: 'a', x: 1, y: 2 }] })).toBe(false)
  })
  it('returns false when the marker is explicitly false', () => {
    expect(isUserArranged({ nodes: [], userArranged: false })).toBe(false)
  })
  it('returns true only when the marker is true', () => {
    expect(isUserArranged({ nodes: [], userArranged: true })).toBe(true)
  })
})

// Re-layout (the topbar button) ignores any stored arrangement and recomputes
// from the auto pass, so the result is then auto again and can be persisted
// without the userArranged marker.
describe('toFlow forceAuto', () => {
  it('ignores stored coordinates when forceAuto is set', () => {
    const stored = {
      nodes: [
        { id: 'start', x: 9999, y: 9999 },
        { id: 'a', x: 9999, y: 9999 },
        { id: 'b', x: 9999, y: 9999 },
        { id: 'c', x: 9999, y: 9999 },
        { id: 'done', x: 9999, y: 9999 },
      ],
    }
    const { nodes } = toFlow(fanOutPlaybook, stored, undefined, undefined, true)
    // Every position must come from the auto pass, so none of the sentinel
    // 9999 stored values should leak through.
    for (const n of nodes) {
      expect(n.position.x).not.toBe(9999)
      expect(n.position.y).not.toBe(9999)
    }
    // The auto pass still respects the fan-out order.
    const x = (id: string) => nodes.find((nd) => nd.id === id)!.position.x
    expect(x('a')).toBeLessThan(x('b'))
    expect(x('b')).toBeLessThan(x('c'))
  })
})
