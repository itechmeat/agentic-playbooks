import type { PlaybookDetail } from './types'
import { edgeAddresses, type AddressedEdge } from './edgeaddress'

// A judge node's exits in the editor: each value a question can write
// (and `decided_by: unavailable` for an `on_unavailable: { route }`) is an
// exit. A value with an outgoing `output_field` edge on this node is that
// edge's exit (`edge:<address>`); a value without one is a hollow handle
// (`new:<field>=<value>`) a person drags to a node to route it.

type PlaybookModel = PlaybookDetail['playbook']
type EdgeModel = PlaybookModel['edges'][number]

/** One value a judge can write on one output field. */
export interface JudgeValue {
  field: string
  value: string
  /** The question it comes from, or `decided_by` for the fallback route. */
  question: string
  /** The rendered address of the edge that routes it, or null. */
  address: string | null
  /** The node `on_unavailable: { route }` names, for the `unavailable` value. */
  route?: string
}

/** The exits a judge node's definition offers. */
export interface JudgeExits {
  values: JudgeValue[]
  /** Hollow handles: values without an edge, grouped by question. */
  hollow: { id: string; label: string; title: string }[]
  /** Notes under the exits: a score without bands, a gap between bands. */
  notes: string[]
}

const NEW_PREFIX = 'new:'

/** The handle id of a value without an edge. */
export function newExitHandle(field: string, value: string): string {
  return `${NEW_PREFIX}${field}=${value}`
}

/** The field and value of a hollow handle id, or null. */
export function parseNewExit(handle: string | null | undefined): { field: string; value: string } | null {
  if (!handle || !handle.startsWith(NEW_PREFIX)) return null
  const rest = handle.slice(NEW_PREFIX.length)
  const eq = rest.indexOf('=')
  if (eq <= 0) return null
  return { field: rest.slice(0, eq), value: rest.slice(eq + 1) }
}

const ENFORCE_NOTE =
  'While the judge_node use is below enforce, a live run takes the on_unavailable route instead of these exits.'
const ORDER_NOTE = 'Conditions are checked in edge order across questions.'

function asMap(v: unknown): Record<string, unknown> | null {
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null
}

function questionEntries(questions: unknown): [string, Record<string, unknown>][] {
  if (Array.isArray(questions)) {
    return questions
      .map((q, i) => [String(asMap(q)?.id ?? `q${i + 1}`), asMap(q) ?? {}] as [string, Record<string, unknown>])
  }
  const m = asMap(questions)
  return m ? Object.entries(m).map(([k, q]) => [k, asMap(q) ?? {}]) : []
}

/** The `[lo, hi]` bands of a score threshold in order, or null. */
function bandsOf(threshold: Record<string, unknown> | null): [string, number, number][] | null {
  const b = asMap(threshold?.bands)
  if (!b) return null
  return Object.entries(b).map(([name, r]) => {
    const pair = Array.isArray(r) ? r : []
    return [name, Number(pair[0]), Number(pair[1])]
  })
}

/** The values each question of a judge writes, in order, and the notes. */
function questionValues(node: Record<string, unknown>): { values: Omit<JudgeValue, 'address'>[]; notes: string[] } {
  const thresholds = asMap(node.thresholds) ?? {}
  const values: Omit<JudgeValue, 'address'>[] = []
  const notes: string[] = []
  for (const [q, spec] of questionEntries(node.questions)) {
    const t = asMap(thresholds[q])
    const kind = spec.type
    const push = (v: string) => values.push({ field: q, value: v, question: q })
    if (kind === 'choice') {
      const keys = Object.keys(asMap(spec.criteria) ?? {})
      keys.forEach(push)
      const below = typeof t?.below === 'string' ? t.below : null
      if (below && !keys.includes(below)) push(below)
    } else if (kind === 'noul') {
      push('true')
      push('false')
    } else if (kind === 'score') {
      const bands = bandsOf(t)
      if (!bands || bands.length === 0) {
        notes.push(`add bands to route on ${q}`)
        continue
      }
      bands.forEach(([name]) => push(name))
      const sorted = [...bands].sort((a, b) => a[1] - b[1])
      const gap = sorted.some((b, i) => i > 0 && b[1] > sorted[i - 1][2])
      if (gap) notes.push(`${q}: values in gaps match no exit; add an else edge`)
    }
  }
  return { values, notes }
}

/** The node `on_unavailable: { route: N }` names, or null. */
export function unavailableRoute(node: Record<string, unknown>): string | null {
  const r = asMap(node.on_unavailable)?.route
  return typeof r === 'string' ? r : null
}

/** Whether an edge routes on `field` equal to `value` of node `nodeId`. */
function routesOn(e: EdgeModel, nodeId: string, field: string, value: string): boolean {
  const c = e.condition
  return (
    !e.fallback &&
    !!c &&
    c.type === 'output_field' &&
    c.node === nodeId &&
    String(c.field) === field &&
    String(c.equals) === value
  )
}

/**
 * The exits of judge node `nodeId`, or null for any other node. Other
 * outgoing edges (a fallback, `node_status`, a condition on another node
 * or field, judge edges) stay ordinary exits of `nodeExits`.
 */
export function judgeExits(
  playbook: PlaybookModel,
  nodeId: string,
  addressed: AddressedEdge[] = edgeAddresses(playbook.edges),
): JudgeExits | null {
  const node = playbook.nodes.find((n) => n.id === nodeId) as Record<string, unknown> | undefined
  if (!node || node.type !== 'judge') return null
  const { values: raw, notes } = questionValues(node)
  const route = unavailableRoute(node)
  if (route) raw.push({ field: 'decided_by', value: 'unavailable', question: 'decided_by', route })
  const values: JudgeValue[] = raw.map((v) => {
    const i = playbook.edges.findIndex((e) => e.from === nodeId && routesOn(e, nodeId, v.field, v.value))
    return { ...v, address: i >= 0 ? addressed[i].rendered : null }
  })
  const hollow = values
    .filter((v) => v.address === null)
    .map((v) => ({
      id: newExitHandle(v.field, v.value),
      label: v.field === 'decided_by' ? 'unavailable' : `${v.field}: ${v.value}`,
      title:
        v.field === 'decided_by'
          ? `not connected: when no answer comes back the judge routes to ${v.route}; drag to a node to set that route. ${ENFORCE_NOTE}`
          : `not connected: ${v.field} = "${v.value}"; drag to a node to route it. ${ORDER_NOTE} ${ENFORCE_NOTE}`,
    }))
  return { values, hollow, notes }
}

/** What a drag from a judge's handle does. */
export type JudgeDrag =
  /** A new `output_field` edge for a value without one. */
  | { kind: 'add'; field: string; value: string }
  /** Retarget the value's existing edge (`to` changes, everything else kept). */
  | { kind: 'retarget'; address: string }
  /** Set `on_unavailable.route` and retarget or create its edge, together. */
  | { kind: 'unavailable' }
  /** Not a judge value: the caller's ordinary rule applies. */
  | { kind: 'other' }

/** What a drag from `handle` of judge node `nodeId` does. */
export function judgeDrag(playbook: PlaybookModel, nodeId: string, handle: string | null | undefined): JudgeDrag {
  const exits = judgeExits(playbook, nodeId)
  if (!exits || !handle) return { kind: 'other' }
  const fresh = parseNewExit(handle)
  if (fresh) {
    if (fresh.field === 'decided_by' && fresh.value === 'unavailable') return { kind: 'unavailable' }
    return { kind: 'add', field: fresh.field, value: fresh.value }
  }
  if (!handle.startsWith('edge:')) return { kind: 'other' }
  const address = handle.slice('edge:'.length)
  const v = exits.values.find((x) => x.address === address)
  if (!v) return { kind: 'other' }
  return v.field === 'decided_by' ? { kind: 'unavailable' } : { kind: 'retarget', address }
}
