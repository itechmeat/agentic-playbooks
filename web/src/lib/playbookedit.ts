import { Document, YAMLSeq } from 'yaml'

// Structural mutations of the playbook model. The source of truth is the YAML
// text, so edits go through the `yaml` package's AST (Document): this preserves
// ALL top-level fields (schema, version, params, executors, defaults,
// supervisor), as well as comments and key order. Each function clones the
// document (immutable from the caller's point of view) and returns a new copy.

import type { NodeType } from './consts.gen'

// The node kinds the editor can add, in the order a graph is usually built,
// with the node that ends a run last. A subset of the schema's node types
// (NodeType is generated from apb-core, so a renamed or removed type fails
// the check here).
export const EDITOR_NODE_KINDS = [
  'start',
  'agent_task',
  'script',
  'condition',
  'playbook',
  'finish',
] as const satisfies readonly NodeType[]
export type NodeKind = (typeof EDITOR_NODE_KINDS)[number]

/** Default value for a new node's title. */
function defaultTitle(kind: string, id: string): string {
  return id
}

/** Guaranteed to return the nodes seq (creates an empty one if missing). */
function ensureNodes(doc: Document): YAMLSeq {
  let nodes = doc.get('nodes')
  if (!nodes) {
    doc.set('nodes', doc.createNode([]))
    nodes = doc.get('nodes')
  }
  return nodes as unknown as YAMLSeq
}

function ensureEdges(doc: Document): YAMLSeq {
  let edges = doc.get('edges')
  if (!edges) {
    doc.set('edges', doc.createNode([]))
    edges = doc.get('edges')
  }
  return edges as unknown as YAMLSeq
}

function nodeById(nodes: YAMLSeq, id: string): ReturnType<YAMLSeq['get']> | undefined {
  return nodes.items.find((n) => (n as { get: (k: string) => unknown }).get('id') === id)
}

/** Adds a node with the given type and id. Returns a new document. */
export function addNode(doc: Document, kind: string, id: string): Document {
  const next = doc.clone()
  const nodes = ensureNodes(next)
  const node = next.createNode({ id, type: kind, title: defaultTitle(kind, id) })
  nodes.add(node)
  return next
}

/** Removes a node and all edges connected to it. Returns a new document. */
export function removeNode(doc: Document, id: string): Document {
  const next = doc.clone()
  const nodes = next.get('nodes')
  if (nodes instanceof YAMLSeq) {
    nodes.items = nodes.items.filter((n) => (n as { get: (k: string) => unknown }).get('id') !== id)
  }
  const edges = next.get('edges')
  if (edges instanceof YAMLSeq) {
    edges.items = edges.items.filter(
      (e) =>
        (e as { get: (k: string) => unknown }).get('from') !== id &&
        (e as { get: (k: string) => unknown }).get('to') !== id,
    )
  }
  return next
}

/**
 * Updates a node's fields with a patch. A value of undefined removes the key.
 * Returns a new document.
 */
export function updateNode(doc: Document, id: string, patch: Record<string, unknown>): Document {
  const next = doc.clone()
  const nodes = next.get('nodes')
  if (!(nodes instanceof YAMLSeq)) return next
  const target = nodeById(nodes, id)
  if (!target) return next
  const map = target as unknown as { set: (k: string, v: unknown) => void; delete: (k: string) => void; has: (k: string) => boolean }
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined) {
      map.delete(key)
    } else {
      map.set(key, next.createNode(value))
    }
  }
  return next
}

/** Adds an edge from->to (with an optional condition). Returns a new document. */
export function addEdge(
  doc: Document,
  from: string,
  to: string,
  condition?: Record<string, unknown>,
): Document {
  const next = doc.clone()
  const edges = ensureEdges(next)
  const edge: Record<string, unknown> = { from, to }
  if (condition) edge.condition = condition
  edges.add(next.createNode(edge))
  return next
}

/** Removes the edge from->to. Returns a new document. */
export function removeEdge(doc: Document, from: string, to: string): Document {
  const next = doc.clone()
  const edges = next.get('edges')
  if (edges instanceof YAMLSeq) {
    edges.items = edges.items.filter(
      (e) =>
        !(
          (e as { get: (k: string) => unknown }).get('from') === from &&
          (e as { get: (k: string) => unknown }).get('to') === to
        ),
    )
  }
  return next
}

/** Suggests a unique id of the form `<kind>-N` for a new node. */
export function suggestNodeId(doc: Document, kind: string): string {
  const existing = new Set<string>()
  const nodes = doc.get('nodes')
  if (nodes instanceof YAMLSeq) {
    for (const n of nodes.items) {
      const id = (n as { get: (k: string) => unknown }).get('id')
      if (typeof id === 'string') existing.add(id)
    }
  }
  let n = 1
  while (existing.has(`${kind}-${n}`)) n++
  return `${kind}-${n}`
}

/** What the editor tells the user after a save (issue #67 item 9). */
export interface SaveNotice {
  kind: 'success' | 'info'
  title: string
  description?: string
}

/**
 * The toast for a save answer: a new playbook, a new version, or - when the
 * definition equaled the current version - that nothing was written.
 */
export function saveNotice(
  result: { id: string; version: string; unchanged?: boolean },
  isNew: boolean,
): SaveNotice {
  if (result.unchanged) {
    return {
      kind: 'info',
      title: 'No changes to save',
      description: `"${result.id}" stays at ${result.version}; no new version was created.`,
    }
  }
  if (isNew) return { kind: 'success', title: `Created "${result.id}"` }
  return { kind: 'success', title: `Saved "${result.id}" as ${result.version}` }
}
