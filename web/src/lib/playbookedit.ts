import { Document, isAlias, isMap as isYamlMap, Scalar, visit, YAMLMap, YAMLSeq, type Alias } from 'yaml'

// Structural mutations of the playbook model. The source of truth is the YAML
// text, so edits go through the `yaml` package's AST (Document): this preserves
// ALL top-level fields (schema, version, params, executors, defaults,
// supervisor), as well as comments and key order. Each function clones the
// document (immutable from the caller's point of view) and returns a new copy.

import type { NodeType } from './consts.gen'
import {
  duplicateEdgeId,
  edgeAddresses,
  renderAddress,
  resolveAddress,
  type AddressableEdge,
  type EdgeAddress,
  type IssueView,
} from './edgeaddress'
import { judgeSkeleton } from './judgefields'
import { publishSkeleton, type SourceNode } from './publish'

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
  'publish',
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

/** The plain nodes of a nodes seq, for the skeletons that look at them. A
 * half-edited item that is not a map with a string id and type is left out. */
function nodesOf(nodes: YAMLSeq): SourceNode[] {
  const plain = nodes.toJSON() as unknown
  if (!Array.isArray(plain)) return []
  return plain.filter(
    (n): n is SourceNode =>
      typeof n === 'object' && n !== null && typeof n.id === 'string' && typeof n.type === 'string',
  )
}

/** The fields a new node of `kind` starts with. A judge node starts with a
 * question skeleton (a bare `type: judge` asks nothing and does not
 * validate); a publish node starts from the last node that can commit and
 * the remote `origin`. */
function skeletonFor(kind: string, nodes: YAMLSeq): Record<string, unknown> {
  if (kind === 'judge') return judgeSkeleton()
  if (kind === 'publish') return publishSkeleton(nodesOf(nodes))
  return {}
}

/** Adds a node with the given type and id. Returns a new document. */
export function addNode(doc: Document, kind: string, id: string): Document {
  const next = doc.clone()
  const nodes = ensureNodes(next)
  const extra = skeletonFor(kind, nodes)
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

/** One edge item of the YAML `edges` with its position in the sequence. */
interface DocEdge extends AddressableEdge {
  /** The position of the item in the `edges` sequence. */
  item: number
}

/**
 * The edges of a document as the address rules read them, each with its
 * position in the sequence. An alias item (`- *e`) is the edge it names, as
 * the parsed playbook reads it, so positions and addresses here match the
 * canvas. An item that is not a map with a string `from` and `to` (a
 * half-edited draft) has no address and is left out.
 */
function docEdges(doc: Document, seq: YAMLSeq): DocEdge[] {
  const out: DocEdge[] = []
  seq.items.forEach((item, at) => {
    const node = isAlias(item) ? item.resolve(doc) : item
    if (!isYamlMap(node)) return
    const plain = node.toJSON() as Record<string, unknown>
    if (typeof plain.from !== 'string' || typeof plain.to !== 'string') return
    out.push({ item: at, id: plain.id, from: plain.from, to: plain.to, condition: plain.condition, fallback: plain.fallback })
  })
  return out
}

/**
 * The id a new edge from `from` to `to` needs, or undefined: an edge that
 * would be a second id-less edge with both ends of an existing one gets an
 * id, so its address does not depend on its position. When every edge of
 * the pair already carries an authored id, the new one is the pair's only
 * id-less edge and needs none. Existing duplicates keep what they have.
 */
function idForPair(edges: DocEdge[], from: string, to: string): string | undefined {
  const idless = edges.filter((e) => e.from === from && e.to === to && typeof e.id !== 'string')
  if (idless.length === 0) return undefined
  const taken = new Set(edges.map((e) => e.id).filter((id): id is string => typeof id === 'string'))
  return duplicateEdgeId(from, to, idless.length + 1, taken)
}

/**
 * Adds an edge from->to (with an optional condition). An unconditional edge
 * is appended; a conditional one goes right before its source's first
 * fallback edge when there is one, so it is checked before the fallback is
 * taken. An edge that shares both ends with an existing one gets an `id`.
 * Returns a new document.
 */
export function addEdge(
  doc: Document,
  from: string,
  to: string,
  condition?: Record<string, unknown>,
): Document {
  const next = doc.clone()
  const seq = ensureEdges(next)
  const existing = docEdges(next, seq)
  const edge: Record<string, unknown> = {}
  const id = idForPair(existing, from, to)
  if (id) edge.id = id
  edge.from = from
  edge.to = to
  if (condition) edge.condition = condition
  const node = next.createNode(edge)
  const fallback = condition ? existing.find((e) => e.from === from && e.fallback === true) : undefined
  if (fallback) seq.items.splice(fallback.item, 0, node)
  else seq.add(node)
  return next
}

/**
 * The position in the `edges` sequence of the edge `address` names, or -1.
 * With `index` (the edge's position in the playbook's `edges`, which is its
 * position in the sequence of a document that parses), the edge there is
 * taken only when it still has that address: two edges sharing an authored
 * id (a validation error) are told apart by it, and a document that changed
 * under a selection removes nothing.
 */
function itemOf(doc: Document, seq: YAMLSeq, address: EdgeAddress | string, index?: number): number {
  const edges = docEdges(doc, seq)
  if (index === undefined) {
    const at = resolveAddress(edges, address)
    return at < 0 ? -1 : edges[at].item
  }
  const at = edges.findIndex((e) => e.item === index)
  if (at < 0) return -1
  const want = typeof address === 'string' ? address : renderAddress(address)
  return edgeAddresses(edges)[at].rendered === want ? index : -1
}

/**
 * Removes exactly the one edge `address` names (an address or its string
 * form), resolved on this document; every other edge, its keys and the
 * comments stay as they are. `index`, the position of the selected edge in
 * the playbook's `edges`, pins the edge when the address alone is
 * ambiguous; see `itemOf`. An address that names no edge changes nothing.
 * An edge other items alias (`&e` ... `*e`) is moved to the first alias
 * before it goes, so they keep their meaning. Returns a new document.
 */
export function removeEdge(doc: Document, address: EdgeAddress | string, index?: number): Document {
  const next = doc.clone()
  const seq = next.get('edges')
  if (!(seq instanceof YAMLSeq)) return next
  const item = itemOf(next, seq, address, index)
  if (item < 0) return next
  const removed = seq.items[item]
  const anchor = isYamlMap(removed) ? removed.anchor : undefined
  if (anchor) {
    let moved = false
    visit(next, {
      Alias(_, alias: Alias) {
        if (moved || alias.source !== anchor) return undefined
        moved = true
        return removed as YAMLMap
      },
    })
  }
  seq.items.splice(item, 1)
  return next
}

/**
 * Adds the edge of a judge exit: `{ from, to, condition: { type:
 * output_field, node: from, field, equals: "<value>" } }`, `equals` always
 * a quoted string, placed and given an id as `addEdge` does.
 */
export function addJudgeExit(doc: Document, from: string, field: string, value: string, to: string): Document {
  const equals = new Scalar(value)
  equals.type = Scalar.QUOTE_DOUBLE
  return addEdge(doc, from, to, { type: 'output_field', node: from, field, equals })
}

/**
 * Points the edge `address` names at `to`: its id, condition and every
 * other key stay. Dropping it on its current target changes nothing. A
 * retarget that makes it an id-less duplicate of a pair gives it an id.
 */
export function retargetEdge(doc: Document, address: EdgeAddress | string, to: string, index?: number): Document {
  const next = doc.clone()
  const seq = next.get('edges')
  if (!(seq instanceof YAMLSeq)) return next
  const item = itemOf(next, seq, address, index)
  if (item < 0) return next
  const raw = seq.items[item]
  const node = isAlias(raw) ? raw.resolve(next) : raw
  if (!isYamlMap(node)) return next
  const plain = node.toJSON() as Record<string, unknown>
  if (plain.to === to) return doc.clone()
  const others = docEdges(next, seq).filter((e) => e.item !== item)
  if (typeof plain.id !== 'string') {
    const id = idForPair(others, String(plain.from), to)
    if (id) node.items.unshift(next.createPair('id', id) as never)
  }
  node.set('to', to)
  return next
}

/**
 * Routes a judge's unavailable answer to `to` in one mutation: sets
 * `on_unavailable: { route: to }` and retargets the `decided_by =
 * unavailable` edge, creating it when it is missing.
 */
export function routeUnavailable(doc: Document, judge: string, to: string): Document {
  let next = doc.clone()
  const nodes = next.get('nodes')
  if (!(nodes instanceof YAMLSeq)) return next
  const node = nodeById(nodes, judge) as YAMLMap | undefined
  if (!node || !isYamlMap(node)) return next
  node.set('on_unavailable', next.createNode({ route: to }))
  const seq = next.get('edges')
  const existing = seq instanceof YAMLSeq ? docEdges(next, seq) : []
  const at = existing.find((e) => {
    const c = e.condition as Record<string, unknown> | undefined
    return (
      e.from === judge &&
      e.fallback !== true &&
      c?.type === 'output_field' &&
      c.node === judge &&
      c.field === 'decided_by' &&
      String(c.equals) === 'unavailable'
    )
  })
  if (!at) return addJudgeExit(next, judge, 'decided_by', 'unavailable', to)
  const address = edgeAddresses(existing)[existing.indexOf(at)].rendered
  next = retargetEdge(next, address, to, at.item)
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

/**
 * The validation issues of a refused save (`{ error: "validation", codes,
 * issues }`); an older server sends only the codes, and then there are none.
 */
export function refusedIssues(body: unknown): IssueView[] {
  if (!isMap(body) || body.error !== 'validation' || !Array.isArray(body.issues)) return []
  return body.issues.filter(
    (i): i is IssueView => isMap(i) && typeof i.code === 'string' && typeof i.message === 'string',
  )
}

/** One issue as a line: its code, message, node and the addresses of its edges. */
export function issueLine(issue: { code: string; message: string; node?: string | null; edges?: string[] }): string {
  const where = [issue.node ? `node ${issue.node}` : '', issue.edges?.length ? `edges ${issue.edges.join(', ')}` : '']
    .filter(Boolean)
    .join('; ')
  return `${issue.code}: ${issue.message}${where ? ` (${where})` : ''}`
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
