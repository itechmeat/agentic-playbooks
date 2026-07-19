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
