import { describe, expect, it } from 'vitest'
import { toFlow } from './graph'

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

  it('auto-layouts nodes left-to-right without stored positions', () => {
    const { nodes } = toFlow(playbook, null)
    const xs = nodes.map((n) => n.position.x)
    // dagre lays out ranks horizontally: start left of a, a left of done
    expect(xs[0]).toBeLessThan(xs[1])
    expect(xs[1]).toBeLessThan(xs[2])
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
})
