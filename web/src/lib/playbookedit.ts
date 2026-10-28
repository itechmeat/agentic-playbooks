import { Document, YAMLSeq } from 'yaml'

// Structural mutations of the playbook model. The source of truth is the YAML
// text, so edits go through the `yaml` package's AST (Document): this preserves
// ALL top-level fields (schema, version, params, executors, defaults,
// supervisor), as well as comments and key order. Each function clones the
// document (immutable from the caller's point of view) and returns a new copy.

import type { NodeType } from './consts.gen'
import { judgeSkeleton } from './judgefields'

// The node kinds the editor can add, in the order a graph is usually built,
// with the node that ends a run last. A subset of the schema's node types
// (NodeType is generated from apb-core, so a renamed or removed type fails
// the check here).
export const EDITOR_NODE_KINDS = [
  'start',
  'agent_task',
  'script',
  'condition',
  'judge',
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
  // A judge node starts with a question skeleton: a bare `type: judge` asks
  // nothing and does not validate.
  const extra = kind === 'judge' ? judgeSkeleton() : {}
  const node = next.createNode({ id, type: kind, title: defaultTitle(kind, id), ...extra })
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

/** The isolation levels the node form offers; '' leaves the key out (none). */
export const ISOLATION_LEVELS = ['', 'none', 'best_effort', 'full'] as const
export type IsolationLevel = (typeof ISOLATION_LEVELS)[number]
/** The networks an isolated node may declare; '' leaves it implicit (open). */
export const ISOLATION_NETWORKS = ['', 'open', 'none'] as const
export type IsolationNetwork = (typeof ISOLATION_NETWORKS)[number]

/** A node's `isolation` as the form shows it, from either YAML form. */
export interface IsolationForm {
  level: IsolationLevel
  network: IsolationNetwork
  /** The level as written when it is not one the form offers (a typo, a newer apb); undefined when it parsed. */
  rawLevel?: string
  /** The network as written when it is not one the form offers; undefined when it parsed. */
  rawNetwork?: string
  /** Extra read-only paths; the form shows them, it does not edit them. */
  readOnly: string[]
  /** Extra writable paths; shown read-only as well. */
  writable: string[]
}

function oneOf<T extends string>(allowed: readonly T[], v: unknown): T {
  return (allowed as readonly unknown[]).includes(v) ? (v as T) : allowed[0]
}

function rawUnknown(allowed: readonly string[], v: unknown): string | undefined {
  if (v === undefined || v === null || allowed.includes(v as string)) return undefined
  return typeof v === 'string' ? v : JSON.stringify(v)
}

function stringList(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
}

function isMap(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/**
 * Reads `isolation` in the shorthand (`full`) or the long map
 * (`{ level, network, read_only, writable }`).
 */
export function isolationForm(raw: unknown): IsolationForm {
  if (isMap(raw)) {
    return {
      level: oneOf(ISOLATION_LEVELS, raw.level),
      network: oneOf(ISOLATION_NETWORKS, raw.network),
      ...unknownFields(raw.level, raw.network),
      readOnly: stringList(raw.read_only),
      writable: stringList(raw.writable),
    }
  }
  return { level: oneOf(ISOLATION_LEVELS, raw), network: '', ...unknownFields(raw, undefined), readOnly: [], writable: [] }
}

function unknownFields(level: unknown, network: unknown): Pick<IsolationForm, 'rawLevel' | 'rawNetwork'> {
  const out: Pick<IsolationForm, 'rawLevel' | 'rawNetwork'> = {}
  const l = rawUnknown(ISOLATION_LEVELS, level)
  const n = rawUnknown(ISOLATION_NETWORKS, network)
  if (l !== undefined) out.rawLevel = l
  if (n !== undefined) out.rawNetwork = n
  return out
}

/** Whether the level isolates the node at all, so a network may be declared. */
export function isIsolatedLevel(level: IsolationLevel): boolean {
  return level === 'full' || level === 'best_effort'
}

/**
 * The new `isolation` value after the form changed the level or the network,
 * built on the current value so nothing the form does not edit is lost: the
 * extra paths and any other key of a long map stay as they are. Picking a
 * level that does not isolate drops the network and the extra paths (the
 * validator refuses them there). A change that picks no level leaves a value
 * whose level does not isolate, or is unknown to the form (a typo, a newer
 * apb), exactly as written. A value with only a level is written as the
 * shorthand; `undefined` removes the key.
 */
export function isolationValue(
  current: unknown,
  change: { level?: IsolationLevel; network?: IsolationNetwork },
): unknown {
  const form = isolationForm(current)
  if (change.level === undefined && (form.rawLevel !== undefined || !isIsolatedLevel(form.level))) {
    return current
  }
  const level = change.level ?? form.level
  const network = isIsolatedLevel(level) ? (change.network ?? form.network) : ''
  const map: Record<string, unknown> = isMap(current) ? { ...current } : {}
  delete map.level
  delete map.network
  if (!isIsolatedLevel(level)) {
    delete map.read_only
    delete map.writable
  }
  const rest = Object.keys(map).length > 0
  if (!rest && network === '') return level === '' ? undefined : level
  const out: Record<string, unknown> = {}
  if (level !== '') out.level = level
  if (network !== '') out.network = network
  return { ...out, ...map }
}
