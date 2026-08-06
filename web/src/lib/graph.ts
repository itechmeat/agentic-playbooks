import dagre from '@dagrejs/dagre'
import type { PlaybookDetail, WfLayout } from './types'

export interface FlowNode {
  id: string
  position: { x: number; y: number }
  data: {
    title: string
    kind: string
    status?: string
    cached?: boolean
    failure?: FailureEffect | null
    exits?: NodeExits | null
  }
  type: 'playbookNode'
}

export interface FlowEdge {
  id: string
  source: string
  target: string
  label?: string
  /** The named exit this edge leaves from; absent when the node has one exit. */
  sourceHandle?: string
}

type PlaybookModel = PlaybookDetail['playbook']
type PlaybookEdgeModel = PlaybookModel['edges'][number]

const NODE_W = 200
const NODE_H = 64
/** Extra layout height for the row of exit captions, so ranks stay clear of it. */
const EXIT_ROW_H = 24
/** Captions sit side by side under a 200px node, so they have to stay short. */
const EXIT_LABEL_MAX = 15

function edgeLabel(e: PlaybookEdgeModel): string | undefined {
  const c = e.condition
  if (!c) return undefined
  if (c.type === 'node_status') return `${c.node}: ${c.equals}`
  if (c.type === 'review_status') return `review: ${c.equals}`
  if (c.type === 'output_match') return `match: ${c.pattern}`
  if (c.type === 'output_field') return `${c.field}: ${c.equals}`
  return c.type
}

/** One named way out of a node, in the order the engine considers it. */
export interface NodeExit {
  /** Handle id, referenced by the edge's `sourceHandle`. */
  id: string
  /** Short caption under the dot. */
  label: string
  /** The unabbreviated meaning, for the tooltip. */
  title: string
  tone: 'default' | 'success' | 'failure'
}

export interface NodeExits {
  /**
   * `all`: every exit is taken and the branches run in parallel (the node has
   * unconditional edges). `one-of`: exactly one exit is taken, the first whose
   * condition matches, which is why the captions are numbered.
   */
  mode: 'all' | 'one-of'
  list: NodeExit[]
}

/**
 * Caps the caption INCLUDING its ordinal prefix, so every exit is the same
 * width. The ellipsis goes in the middle, not at the end: two exits whose
 * captions share a long prefix would otherwise truncate to the same text and
 * become indistinguishable, which is the very confusion these captions exist
 * to remove.
 */
function truncate(s: string, budget: number): string {
  if (s.length <= budget) return s
  const head = Math.ceil((budget - 1) / 2)
  return `${s.slice(0, head)}…${s.slice(s.length - (budget - 1 - head))}`
}

function exitCaption(
  e: PlaybookEdgeModel,
  nodeId: string,
): { label: string; title: string; tone: NodeExit['tone'] } {
  if (e.fallback) {
    return {
      label: 'else',
      title: `fallback: taken when no other condition matched, goes to ${e.to}`,
      tone: 'default',
    }
  }
  const c = e.condition
  if (!c) {
    return {
      label: e.to,
      title: `always taken, in parallel with this node's other unconditional exits, goes to ${e.to}`,
      tone: 'default',
    }
  }
  if (c.type === 'node_status') {
    const own = c.node === nodeId
    const equals = String(c.equals)
    return {
      label: own ? equals : `${c.node}: ${equals}`,
      title: own
        ? `taken when this node reports ${equals}, goes to ${e.to}`
        : `taken when node ${c.node} reports ${equals}, goes to ${e.to}`,
      tone: equals === 'failure' ? 'failure' : 'success',
    }
  }
  if (c.type === 'review_status') {
    const equals = String(c.equals)
    return {
      label: equals,
      title: `taken when this review is decided ${equals}, goes to ${e.to}`,
      tone: 'default',
    }
  }
  if (c.type === 'output_match') {
    const own = c.node === nodeId
    const pattern = String(c.pattern)
    // Patterns are usually `key: value` markers an agent emits, and it is the
    // value that tells two exits apart, so the caption keeps that and the
    // tooltip keeps the whole pattern.
    const split = pattern.lastIndexOf(': ')
    const short = split >= 0 && split + 2 < pattern.length ? pattern.slice(split + 2) : pattern
    return {
      label: `match: ${short}`,
      title: own
        ? `taken when this node's output contains "${pattern}", goes to ${e.to}`
        : `taken when node ${c.node}'s output contains "${pattern}", goes to ${e.to}`,
      tone: 'default',
    }
  }
  if (c.type === 'output_field') {
    const own = c.node === nodeId
    const field = String(c.field)
    const equals = String(c.equals)
    // Sibling exits normally read the SAME field and differ only in the value,
    // so the value has to survive truncation; the field name leads because it
    // is what says which verdict is being routed on.
    return {
      label: `${field}: ${equals}`,
      title: own
        ? `taken when this node's output is a JSON object whose "${field}" equals "${equals}", goes to ${e.to}`
        : `taken when node ${c.node}'s output is a JSON object whose "${field}" equals "${equals}", goes to ${e.to}`,
      tone: 'default',
    }
  }
  return { label: String(c.type), title: `${c.type}, goes to ${e.to}`, tone: 'default' }
}

/**
 * The named exits of a node, or null when it has fewer than two and one
 * anonymous dot at the bottom says everything there is to say.
 *
 * Several edges leaving one point hide two opposite meanings: unconditional
 * edges all run, conditional ones pick exactly one. They also hide that the
 * pick is first-match, so declaration order decides the route. Both become
 * visible here: a mode, and a numbered caption per exit.
 */
export function nodeExits(playbook: PlaybookModel, nodeId: string): NodeExits | null {
  const out = playbook.edges.filter((e) => e.from === nodeId)
  if (out.length < 2) return null
  // Mirrors the engine's edge selection: any unconditional edge is taken
  // regardless of status, so its presence makes the whole set a fan-out.
  const mode: NodeExits['mode'] = out.some((e) => !e.condition && !e.fallback) ? 'all' : 'one-of'
  return {
    mode,
    list: out.map((e, i) => {
      const { label, title, tone } = exitCaption(e, nodeId)
      const ordinal = mode === 'one-of' ? `${i + 1} ` : ''
      return {
        id: `out-${i}`,
        label: ordinal + truncate(label, EXIT_LABEL_MAX - ordinal.length),
        title:
          mode === 'one-of' ? `checked ${i + 1} of ${out.length} in order: ${title}` : title,
        tone,
      }
    }),
  }
}

// The node kinds that can end `failed` or `timed_out`: the three that run an
// attempt, plus `wait`, which fails when its own timeout elapses before the
// signal. A start cannot fail, a condition and a prompt are evaluated rather
// than executed, and a finish has no outgoing edge to begin with.
//
// KNOWN GAP: a join node also fails when an upstream branch fails, whatever its
// own kind, so a `condition` used as a join is left unmarked here even though
// the policy does handle its failure. Closing that needs the edge `join` field,
// which the editor's client-side parse does not carry today.
const FAILABLE_KINDS = new Set(['agent_task', 'script', 'playbook', 'wait'])

/** What the playbook's failure policy does for one node, when it applies. */
export type FailureEffect = { kind: 'stop' } | { kind: 'route'; node: string }

/**
 * What happens if this node fails and no edge takes that failure, or null when
 * the question does not arise. The canvas has to show it, or the graph reads as
 * if the failure case was simply forgotten.
 *
 * Deliberately conservative: only an edge conditioned on THIS node succeeding
 * is known to be unable to carry its failure. An unconditional edge is taken
 * whatever the status, a `fallback` edge exists to catch exactly this, an
 * `output_match` or `output_field` may still match a failed node's output, and
 * a condition on another node's status says nothing about this one. Anything the frontend
 * cannot decide statically stays unmarked, so the marker never claims more
 * than it knows.
 *
 * Note that the policy governs autonomous runs. A supervised run parks a failed
 * node for its supervisor instead and never consults this route.
 */
export function failureEffect(playbook: PlaybookModel, nodeId: string): FailureEffect | null {
  const policy = playbook.defaults?.on_failure
  if (!policy || policy === 'route') return null
  const stops = policy === 'stop'
  // The handler is never routed to itself, so nothing is claimed on it. Only
  // for the route form: `stop` is a reserved word, not a node reference, so a
  // node that happens to be named `stop` still stops the run like any other.
  if (!stops && policy === nodeId) return null
  // A route to a node that does not exist is a V35 validation error; naming it
  // on the canvas would just repeat the mistake as if it were a plan.
  if (!stops && !playbook.nodes.some((n) => n.id === policy)) return null
  const node = playbook.nodes.find((n) => n.id === nodeId)
  if (!node || !FAILABLE_KINDS.has(node.type)) return null
  const out = playbook.edges.filter((e) => e.from === nodeId)
  if (out.length === 0) return null
  const unhandled = out.every(
    (e) =>
      !e.fallback &&
      e.condition?.type === 'node_status' &&
      e.condition.node === nodeId &&
      e.condition.equals === 'success',
  )
  if (!unhandled) return null
  return stops ? { kind: 'stop' } : { kind: 'route', node: policy }
}

/**
 * True only when the layout was explicitly arranged by hand. The marker is
 * additive: layouts written before it existed parse as "auto", so existing
 * call sites keep their behavior.
 */
export function isUserArranged(layout: WfLayout | null): boolean {
  return !!layout?.userArranged
}

/**
 * For each source node with several distinct successors, constrain dagre's
 * order phase so the successors land left-to-right in the order the source's
 * out-edges are declared (the order `nodeExits` numbers). dagre's balance
 * phase otherwise flips tied siblings, which made a simple fan-out read
 * right-to-left and the branch arrows cross (issue #68 item 2). Constraints
 * only apply between distinct successors of the same source; dagre ignores
 * a constraint whose endpoints land in different ranks.
 */
function fanoutConstraints(playbook: PlaybookModel): { left: string; right: string }[] {
  const constraints: { left: string; right: string }[] = []
  for (const node of playbook.nodes) {
    const seen = new Set<string>()
    const distinct: string[] = []
    for (const e of playbook.edges) {
      if (e.from !== node.id) continue
      if (seen.has(e.to)) continue
      seen.add(e.to)
      distinct.push(e.to)
    }
    for (let i = 0; i < distinct.length - 1; i++) {
      constraints.push({ left: distinct[i], right: distinct[i + 1] })
    }
  }
  return constraints
}

export function toFlow(
  playbook: PlaybookModel,
  layout: WfLayout | null,
  statuses?: Record<string, string>,
  cachedIds?: Set<string>,
  forceAuto?: boolean,
): { nodes: FlowNode[]; edges: FlowEdge[] } {
  // `forceAuto` is the topbar "Re-layout" button: it ignores any stored
  // arrangement (manual or otherwise) and recomputes from scratch, so the
  // result is auto again and can be persisted without the userArranged marker.
  const stored = new Map<string, { x: number; y: number }>()
  if (!forceAuto) {
    for (const n of layout?.nodes ?? []) stored.set(n.id, { x: n.x, y: n.y })
  }

  const exits = new Map<string, NodeExits | null>(
    playbook.nodes.map((n) => [n.id, nodeExits(playbook, n.id)]),
  )

  const needAuto = forceAuto || playbook.nodes.some((n) => !stored.has(n.id))
  const auto = new Map<string, { x: number; y: number }>()
  if (needAuto) {
    const g = new dagre.graphlib.Graph()
    // TB: the playbook is laid out vertically, top to bottom. Branches are
    // usually few, and a vertical flow reads better on narrow / mobile screens.
    g.setGraph({ rankdir: 'TB', nodesep: 40, ranksep: 80 })
    g.setDefaultEdgeLabel(() => ({}))
    for (const n of playbook.nodes) {
      // A node with named exits is taller by its caption row; laying it out at
      // the plain height would let the next rank sit on top of the captions.
      g.setNode(n.id, { width: NODE_W, height: NODE_H + (exits.get(n.id) ? EXIT_ROW_H : 0) })
    }
    for (const e of playbook.edges) g.setEdge(e.from, e.to)
    const constraints = fanoutConstraints(playbook)
    dagre.layout(g, constraints.length ? { constraints } : {})
    for (const n of playbook.nodes) {
      const pos = g.node(n.id)
      auto.set(n.id, { x: pos.x - NODE_W / 2, y: pos.y - NODE_H / 2 })
    }
  }

  const nodes: FlowNode[] = playbook.nodes.map((n) => ({
    id: n.id,
    type: 'playbookNode',
    position: stored.get(n.id) ?? auto.get(n.id) ?? { x: 0, y: 0 },
    data: {
      title: n.title ?? n.id,
      kind: n.type,
      status: statuses?.[n.id],
      cached: cachedIds?.has(n.id),
      failure: failureEffect(playbook, n.id),
      exits: exits.get(n.id) ?? null,
    },
  }))

  // Each edge leaves from its own named exit when the node has several, in the
  // same order `nodeExits` numbered them.
  const seen = new Map<string, number>()
  const edges: FlowEdge[] = playbook.edges.map((e, i) => {
    const index = seen.get(e.from) ?? 0
    seen.set(e.from, index + 1)
    const named = exits.get(e.from)
    return {
      id: `e${i}-${e.from}-${e.to}`,
      source: e.from,
      target: e.to,
      // The condition is written under its exit now, so repeating it halfway
      // along the line would only add clutter.
      label: named ? undefined : edgeLabel(e),
      ...(named ? { sourceHandle: named.list[index]?.id } : {}),
    }
  })

  return { nodes, edges }
}
