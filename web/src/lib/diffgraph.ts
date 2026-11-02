import type { VersionDiff } from './api.gen'
import { edgeAddresses, renderAddress, type EdgeAddress } from './edgeaddress'
import { edgeLabel, edgeTone, toFlow, type FlowEdge, type FlowNode } from './graph'
import type { PlaybookDetail } from './types'

// The version diff on one canvas: the union of two definitions. Its edges
// are named by display ids from the correspondence (`c<row>` for an edge
// of both, `before:<row>` for a removed one, `after:<row>` for an added
// one); the addresses are data on each edge, never the canvas id, since one
// address can name different edges in the two versions.

type PlaybookModel = PlaybookDetail['playbook']
type EdgeModel = PlaybookModel['edges'][number]

/** How a node or an edge differs between the two versions. */
export type DiffMark = NonNullable<FlowNode['data']['diff']>

function edgeAt(model: PlaybookModel, address: EdgeAddress): EdgeModel | undefined {
  const want = renderAddress(address)
  const at = edgeAddresses(model.edges).find((a) => a.rendered === want)
  return at ? model.edges[at.index] : undefined
}

/**
 * The union canvas of `before` and `after`: every node of `after`, then the
 * removed ones as ghosts; one edge per correspondence row. Named exits are
 * left out (a diff reads the lines, not the handles), and a one-of source
 * whose edges are checked in another order gets a badge.
 */
export function diffUnion(
  before: PlaybookModel,
  after: PlaybookModel,
  diff: Pick<VersionDiff, 'nodes_added' | 'nodes_removed' | 'nodes_changed' | 'edges' | 'edge_order_changed'>,
): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const removed = new Set(diff.nodes_removed)
  const added = new Set(diff.nodes_added)
  const changed = new Set(diff.nodes_changed)
  const reordered = new Set(diff.edge_order_changed.map((c) => c.node))

  const rows: { id: string; edge: EdgeModel; mark?: DiffMark; changed: string[]; before?: string; after?: string }[] = []
  diff.edges.forEach((row, i) => {
    const b = row.before ? edgeAt(before, row.before) : undefined
    const a = row.after ? edgeAt(after, row.after) : undefined
    const addr = {
      ...(row.before ? { before: renderAddress(row.before) } : {}),
      ...(row.after ? { after: renderAddress(row.after) } : {}),
    }
    if (a && b) {
      rows.push({ id: `c${i}`, edge: a, ...(row.changed.length ? { mark: 'changed' } : {}), changed: row.changed, ...addr })
    } else if (a) {
      rows.push({ id: `after:${i}`, edge: a, mark: 'added', changed: [], ...addr })
    } else if (b) {
      rows.push({ id: `before:${i}`, edge: b, mark: 'removed', changed: [], ...addr })
    }
  })

  const union: PlaybookModel = {
    ...after,
    nodes: [...after.nodes, ...before.nodes.filter((n) => removed.has(n.id))],
    edges: rows.map((r) => r.edge),
  }
  const flow = toFlow(union)
  const nodes = flow.nodes.map((n) => {
    const mark: DiffMark | undefined = removed.has(n.id)
      ? 'removed'
      : added.has(n.id)
        ? 'added'
        : changed.has(n.id)
          ? 'changed'
          : undefined
    const { exits: _exits, ...data } = n.data
    return {
      ...n,
      data: {
        ...data,
        exits: null,
        ...(mark ? { diff: mark } : {}),
        ...(reordered.has(n.id) ? { orderChanged: true } : {}),
      },
    }
  })
  const edges: FlowEdge[] = rows.map((r) => {
    const base = edgeLabel(r.edge)
    const label = r.mark === 'changed' ? [base, `changed: ${r.changed.join(', ')}`].filter(Boolean).join(' · ') : base
    return {
      id: r.id,
      source: r.edge.from,
      target: r.edge.to,
      ...(label ? { label } : {}),
      data: {
        tone: edgeTone(r.edge),
        ...(r.mark ? { diff: r.mark } : {}),
        ...(r.before ? { before: r.before } : {}),
        ...(r.after ? { after: r.after } : {}),
      },
    }
  })
  return { nodes, edges }
}
