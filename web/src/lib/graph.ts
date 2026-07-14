import dagre from '@dagrejs/dagre'
import type { PlaybookDetail } from './types'

export interface FlowNode {
  id: string
  position: { x: number; y: number }
  data: { title: string; kind: string; status?: string }
  type: 'playbookNode'
}

export interface FlowEdge {
  id: string
  source: string
  target: string
  label?: string
}

type PlaybookModel = PlaybookDetail['playbook']
type WfLayout = PlaybookDetail['layout']

const NODE_W = 200
const NODE_H = 64

function edgeLabel(e: PlaybookModel['edges'][number]): string | undefined {
  const c = e.condition
  if (!c) return undefined
  if (c.type === 'node_status') return `${c.node}: ${c.equals}`
  if (c.type === 'review_status') return `review: ${c.equals}`
  if (c.type === 'output_match') return `match: ${c.pattern}`
  return c.type
}

export function toFlow(
  playbook: PlaybookModel,
  layout: WfLayout,
  statuses?: Record<string, string>,
): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const stored = new Map<string, { x: number; y: number }>()
  for (const n of layout?.nodes ?? []) stored.set(n.id, { x: n.x, y: n.y })

  const needAuto = playbook.nodes.some((n) => !stored.has(n.id))
  const auto = new Map<string, { x: number; y: number }>()
  if (needAuto) {
    const g = new dagre.graphlib.Graph()
    // LR: the playbook is laid out horizontally, left to right.
    g.setGraph({ rankdir: 'LR', nodesep: 40, ranksep: 80 })
    g.setDefaultEdgeLabel(() => ({}))
    for (const n of playbook.nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H })
    for (const e of playbook.edges) g.setEdge(e.from, e.to)
    dagre.layout(g)
    for (const n of playbook.nodes) {
      const pos = g.node(n.id)
      auto.set(n.id, { x: pos.x - NODE_W / 2, y: pos.y - NODE_H / 2 })
    }
  }

  const nodes: FlowNode[] = playbook.nodes.map((n) => ({
    id: n.id,
    type: 'playbookNode',
    position: stored.get(n.id) ?? auto.get(n.id) ?? { x: 0, y: 0 },
    data: { title: n.title ?? n.id, kind: n.type, status: statuses?.[n.id] },
  }))

  const edges: FlowEdge[] = playbook.edges.map((e, i) => ({
    id: `e${i}-${e.from}-${e.to}`,
    source: e.from,
    target: e.to,
    label: edgeLabel(e),
  }))

  return { nodes, edges }
}
