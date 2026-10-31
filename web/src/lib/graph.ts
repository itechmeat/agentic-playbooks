import dagre from '@dagrejs/dagre'
import type { PlaybookDetail, WfLayout } from './types'
import { profileToField, type ProfileScope } from './profileref'

/**
 * What a graph card (PlaybookNode.svelte) shows. `toFlow` fills the playbook
 * fields; the run fields are optional and left to the run view, which knows
 * the live attempt. Every field past `kind` is optional and a card renders
 * whatever subset it gets.
 */
export type FlowNodeData = {
  /** The node's title, or its id when it has none. */
  title: string
  /** The node type (`agent_task`, `judge`, `human_review`, `start`, ...). */
  kind: string
  /** Node status in a run (`running`, `succeeded`, `failed`, `lost`, ...). */
  status?: string
  cached?: boolean
  failure?: FailureEffect | null
  exits?: NodeExits | null
  // --- playbook fields, filled by `toFlow` / `withProfileModels` ---
  /** Executor profile name: the node's own, else the playbook default (agent tasks). */
  profile?: string
  /** The explicit scope of that profile ref (`{ name, scope }` or `scope/name`); absent for scope auto. */
  profileScope?: ProfileScope
  /** The profile's model, e.g. `opus 5.5`; filled by `withProfileModels`. */
  model?: string
  /** Expected time of one execution as written, e.g. `30m`. */
  expected?: string
  /** Id of the sub-playbook a `playbook` node runs. */
  subPlaybook?: string
  // --- run fields, optional, filled by the run view ---
  /** Progress of the running attempt, 0 to 100; draws the hatched bar. */
  progress?: number
  /** Attempt number of the running or last attempt (1 based). */
  attempt?: number
  /** Human duration of the finished node, e.g. `3m 12s`. */
  duration?: string
  /** Short note after the status, e.g. `#205` or `no · 0.91`. */
  note?: string
}

export interface FlowNode {
  id: string
  position: { x: number; y: number }
  data: FlowNodeData
  type: 'playbookNode'
  /** Set by a page to make the card glow as the selected one. */
  selected?: boolean
}

/**
 * How an edge reads on the canvas: `fail` carries a failure (dashed red),
 * `else` is a fallback (faint), `cond` has a condition, `plain` is always
 * taken.
 */
export type EdgeTone = 'plain' | 'cond' | 'fail' | 'else'

/**
 * What the edge component (PlaybookEdge.svelte) reads. `state` is optional
 * and only set by a run view: `walked` was traversed, `active` leads to the
 * running node, `idle` was not reached (dimmed). Absent: the plain look.
 */
export type FlowEdgeData = {
  tone: EdgeTone
  state?: 'walked' | 'active' | 'idle'
}

export interface FlowEdge {
  id: string
  source: string
  target: string
  label?: string
  /** The named exit this edge leaves from; absent when the node has one exit. */
  sourceHandle?: string
  data: FlowEdgeData
}

type PlaybookModel = PlaybookDetail['playbook']
type PlaybookEdgeModel = PlaybookModel['edges'][number]

const NODE_W = 210
const NODE_H = 76
/** Extra layout height for the row of exit captions, so ranks stay clear of it. */
const EXIT_ROW_H = 24
/** Start and finish render as small pills (see PlaybookNode.svelte). */
const PILL_W = 110
const PILL_H = 30

/**
 * Layout box of one card. A node with named exits is taller by its caption
 * row; laying it out at the plain height would let the next rank sit on top
 * of the captions. Start and finish without exits are pills.
 */
function nodeSize(kind: string, hasExits: boolean): { width: number; height: number } {
  if ((kind === 'start' || kind === 'finish') && !hasExits) return { width: PILL_W, height: PILL_H }
  return { width: NODE_W, height: NODE_H + (hasExits ? EXIT_ROW_H : 0) }
}

/** Captions sit side by side under a 200px node, so they have to stay short. */
const EXIT_LABEL_MAX = 15

/** The look of one edge, from its fallback flag and condition. */
export function edgeTone(e: PlaybookEdgeModel): EdgeTone {
  if (e.fallback) return 'else'
  const c = e.condition
  if (!c) return 'plain'
  if (c.type === 'node_status' && String(c.equals) === 'failure') return 'fail'
  return 'cond'
}

/**
 * A node's `profile` field as name and explicit scope: a bare name (scope
 * auto), `scope/name`, or `{ name, scope }`.
 */
function profileRefOf(v: unknown): { name: string; scope?: ProfileScope } | undefined {
  const field = profileToField(v)
  if (!field) return undefined
  const slash = field.lastIndexOf('/')
  if (slash < 0) return { name: field }
  const name = field.slice(slash + 1)
  const scope = field.slice(0, slash)
  if (!name) return undefined
  return scope === 'project' || scope === 'global' ? { name, scope } : { name }
}

/** `expected_duration` as written: a duration string, or seconds as a number. */
export function expectedText(v: unknown): string | undefined {
  if (typeof v === 'string') return v.trim() || undefined
  if (typeof v !== 'number' || !Number.isFinite(v) || v <= 0) return undefined
  if (v % 3600 === 0) return `${v / 3600}h`
  if (v % 60 === 0) return `${v / 60}m`
  return `${v}s`
}

export function edgeLabel(e: PlaybookEdgeModel): string | undefined {
  const c = e.condition
  if (!c) return undefined
  if (c.type === 'node_status') return `${c.node}: ${c.equals}`
  if (c.type === 'review_status') return `review: ${c.equals}`
  if (c.type === 'output_match') return `match: ${c.pattern}`
  if (c.type === 'output_field') return `${c.field}: ${c.equals}`
  if (c.type === 'judge') return `judge p>=${c.min_p}`
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
  if (c.type === 'judge') {
    const fallback = c.on_unavailable === true ? 'taken' : 'not taken'
    return {
      label: `judge: ${String(c.min_p)}`,
      title: `taken when a decision model answers yes to "${String(c.question)}" with p >= ${String(c.min_p)}; without an answer (no provider, the judge_edge use below enforce) it is ${fallback}; goes to ${e.to}`,
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
// A `judge` fails when no answer comes back and its on_unavailable is `fail`.
const FAILABLE_KINDS = new Set(['agent_task', 'script', 'playbook', 'wait', 'judge'])

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
  // left -> rights already constrained. Two sources can declare the same pair
  // of successors in opposite order (A exits to x then y, B to y then x); a
  // cycle in the constraint graph breaks dagre's order phase, which then
  // stacks two nodes on one spot and throws "Not possible to find
  // intersection inside of the rectangle". The first declaration wins and a
  // constraint that would close a cycle is dropped.
  const after = new Map<string, Set<string>>()
  const reaches = (from: string, to: string): boolean => {
    const stack = [from]
    const visited = new Set<string>()
    while (stack.length) {
      const cur = stack.pop()!
      if (cur === to) return true
      if (visited.has(cur)) continue
      visited.add(cur)
      for (const next of after.get(cur) ?? []) stack.push(next)
    }
    return false
  }
  const add = (left: string, right: string) => {
    if (left === right || reaches(right, left)) return
    if (after.get(left)?.has(right)) return
    if (!after.has(left)) after.set(left, new Set())
    after.get(left)!.add(right)
    constraints.push({ left, right })
  }
  for (const node of playbook.nodes) {
    const seen = new Set<string>()
    const distinct: string[] = []
    for (const e of playbook.edges) {
      if (e.from !== node.id) continue
      if (seen.has(e.to)) continue
      seen.add(e.to)
      distinct.push(e.to)
    }
    for (let i = 0; i < distinct.length - 1; i++) add(distinct[i], distinct[i + 1])
  }
  return constraints
}

type Pos = { x: number; y: number }

/** One dagre pass; throws whatever dagre throws. */
function dagreLayout(
  playbook: PlaybookModel,
  exits: Map<string, NodeExits | null>,
  constraints: { left: string; right: string }[],
): Map<string, Pos> {
  const g = new dagre.graphlib.Graph()
  // TB: the playbook is laid out vertically, top to bottom. Branches are
  // usually few, and a vertical flow reads better on narrow / mobile screens.
  g.setGraph({ rankdir: 'TB', nodesep: 30, ranksep: 70 })
  g.setDefaultEdgeLabel(() => ({}))
  const ids = new Set(playbook.nodes.map((n) => n.id))
  for (const n of playbook.nodes) g.setNode(n.id, nodeSize(n.type, !!exits.get(n.id)))
  // An edge to a node that does not exist (a draft being edited) would make
  // graphlib invent a node without a size, so it is left out of the layout.
  for (const e of playbook.edges) if (ids.has(e.from) && ids.has(e.to)) g.setEdge(e.from, e.to)
  dagre.layout(g, constraints.length ? { constraints } : {})
  const out = new Map<string, Pos>()
  for (const n of playbook.nodes) {
    const pos = g.node(n.id)
    if (!pos || !Number.isFinite(pos.x) || !Number.isFinite(pos.y)) throw new Error(`no position for ${n.id}`)
    out.set(n.id, { x: pos.x - pos.width / 2, y: pos.y - pos.height / 2 })
  }
  return out
}

/**
 * Last resort when dagre throws: breadth-first ranks from the nodes nothing
 * leads to (else the first node), one row per rank in declaration order.
 * Deterministic and never fails, so the page always draws something.
 */
function gridLayout(playbook: PlaybookModel): Map<string, Pos> {
  const ids = playbook.nodes.map((n) => n.id)
  const known = new Set(ids)
  const edges = playbook.edges.filter((e) => known.has(e.from) && known.has(e.to))
  const hasIncoming = new Set(edges.filter((e) => e.from !== e.to).map((e) => e.to))
  const rank = new Map<string, number>()
  const queue: string[] = ids.filter((id) => !hasIncoming.has(id))
  if (!queue.length && ids.length) queue.push(ids[0])
  for (const id of queue) rank.set(id, 0)
  const visit = () => {
    while (queue.length) {
      const cur = queue.shift()!
      for (const e of edges) {
        if (e.from !== cur || rank.has(e.to)) continue
        rank.set(e.to, rank.get(cur)! + 1)
        queue.push(e.to)
      }
    }
  }
  visit()
  // Nodes only reachable through a cycle with no entry: start a new rank.
  for (const id of ids) {
    if (rank.has(id)) continue
    rank.set(id, Math.max(-1, ...rank.values()) + 1)
    queue.push(id)
    visit()
  }
  const rowH = NODE_H + EXIT_ROW_H + 70
  const colW = NODE_W + 30
  const used = new Map<number, number>()
  const out = new Map<string, Pos>()
  for (const id of ids) {
    const r = rank.get(id)!
    const col = used.get(r) ?? 0
    used.set(r, col + 1)
    out.set(id, { x: col * colW, y: r * rowH })
  }
  return out
}

/**
 * Auto positions for every node. Layout must never fail the page: if dagre
 * throws with the fan-out ordering constraints it is retried without them,
 * and if it still throws a plain grid is used.
 */
function autoLayout(playbook: PlaybookModel, exits: Map<string, NodeExits | null>): Map<string, Pos> {
  const constraints = fanoutConstraints(playbook)
  try {
    return dagreLayout(playbook, exits, constraints)
  } catch {
    // fall through
  }
  if (constraints.length) {
    try {
      return dagreLayout(playbook, exits, [])
    } catch {
      // fall through
    }
  }
  return gridLayout(playbook)
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
    for (const [id, pos] of autoLayout(playbook, exits)) auto.set(id, pos)
  }

  // Agent tasks without their own profile run on the playbook default.
  const defaults = playbook.defaults as { profile?: unknown } | null | undefined
  const defaultProfile = profileRefOf(defaults?.profile)

  const nodes: FlowNode[] = playbook.nodes.map((n) => {
    const own = profileRefOf(n.profile)
    const ref = own ?? (n.type === 'agent_task' ? defaultProfile : undefined)
    const profile = ref?.name
    const sub = n.type === 'playbook' ? playbookId(n.playbook) : undefined
    return {
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
        ...(profile ? { profile } : {}),
        ...(ref?.scope ? { profileScope: ref.scope } : {}),
        ...(expectedText(n.expected_duration) ? { expected: expectedText(n.expected_duration) } : {}),
        ...(sub ? { subPlaybook: sub } : {}),
      },
    }
  })

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
      data: { tone: edgeTone(e) },
    }
  })

  return { nodes, edges }
}

/** The id of a `playbook` node's reference: `{ id, scope }` or `scope/id`. */
function playbookId(v: unknown): string | undefined {
  if (typeof v === 'string') return v.slice(v.lastIndexOf('/') + 1) || undefined
  if (v && typeof v === 'object' && typeof (v as { id?: unknown }).id === 'string')
    return (v as { id: string }).id || undefined
  return undefined
}

/** One profile as the profile listing returns it (only what the graph needs). */
export interface ProfileModelEntry {
  name: string
  model: string
  scope?: string
  workspace_id?: string
}

/** The key of a profile in a `profileModelMap`: the bare name for scope auto, `scope:name` for an explicit scope. */
export function profileModelKey(name: string, scope?: ProfileScope): string {
  return scope ? `${scope}:${name}` : name
}

/**
 * Profile to model, for one workspace. The bare name resolves the way scope
 * `auto` does (the workspace's own project profile wins over a global one of
 * the same name); `project:<name>` and `global:<name>` (see
 * `profileModelKey`) answer a ref with an explicit scope.
 */
export function profileModelMap(
  profiles: ProfileModelEntry[],
  workspace = '',
): Map<string, string> {
  const map = new Map<string, string>()
  const rank = (p: ProfileModelEntry) =>
    p.scope === 'project' ? (p.workspace_id === workspace ? 2 : -1) : 1
  const best = new Map<string, number>()
  for (const p of profiles) {
    const r = rank(p)
    if (r < 0 || !p.model) continue
    map.set(profileModelKey(p.name, r === 2 ? 'project' : 'global'), p.model)
    if ((best.get(p.name) ?? 0) >= r) continue
    best.set(p.name, r)
    map.set(p.name, p.model)
  }
  return map
}

/** Fills `data.model` of every node whose profile (in its scope) has a known model. */
export function withProfileModels(nodes: FlowNode[], models: Map<string, string>): FlowNode[] {
  return nodes.map((n) => {
    const model = n.data.profile ? models.get(profileModelKey(n.data.profile, n.data.profileScope)) : undefined
    return model && model !== n.data.model ? { ...n, data: { ...n.data, model } } : n
  })
}
