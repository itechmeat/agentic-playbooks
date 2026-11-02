import type {
  Alignment,
  EdgeAddress,
  EdgeRun,
  ExplainClass,
  JudgeEdgeAnswer,
  JudgeNodeAnswers,
  NodeRun,
  NotRun,
  OverlayCandidate,
  PlaybookOverlay,
  RunGraph,
  SegmentMapping,
  TraversalRow,
} from './api.gen'
import { renderAddress, sameAddress } from './edgeaddress'

// What a run proves about a graph, as the canvas draws it: the run page
// (its own graph over the definition that ran) and the playbook page's
// "last run" layer (a run projected onto the shown version). Every edge
// state comes from `RunGraph.edges`, never from endpoint statuses.

/** The run graph, or null when there is none or its text is withheld. */
export function graphOf(g: RunGraph | string | null | undefined): RunGraph | null {
  return g && typeof g === 'object' ? g : null
}

/** Whether the server withheld the graph (a string in its place). */
export function graphWithheld(g: RunGraph | string | null | undefined): boolean {
  return typeof g === 'string'
}

/** How one edge reads on a run canvas. */
export interface EdgeRunLook {
  state: 'taken' | 'not_taken' | 'not_established'
  /** Deliveries over it, shown only above one; never for not established. */
  count?: number
  /** The tooltip. */
  title: string
  /** Decoration: a delivery that leads into a node running now. Taken only. */
  live?: boolean
  /** A judge edge's answer: `p` against `min_p`, or `unknown`. */
  judge?: string
}

/** The look of one established edge state, with an optional judge answer. */
export function edgeLook(e: Pick<EdgeRun, 'state'> & Partial<{ count: number; reason: string }>): EdgeRunLook {
  if (e.state === 'taken') {
    const count = e.count ?? 0
    return {
      state: 'taken',
      ...(count > 1 ? { count } : {}),
      title: count === 1 ? 'taken once' : `taken ${count} times`,
    }
  }
  if (e.state === 'not_taken') return { state: 'not_taken', title: 'not taken' }
  return { state: 'not_established', title: `not established: ${e.reason ?? 'unknown'}` }
}

/** The text on an edge: its count above one and a judge answer. */
export function edgeRunLabel(look: EdgeRunLook | undefined): string | undefined {
  if (!look) return undefined
  const parts = [look.count !== undefined ? `x${look.count}` : '', look.judge ?? ''].filter(Boolean)
  return parts.length ? parts.join(' ') : undefined
}

/** The latest answer per judge edge, by rendered address. */
function judgeEdgeLabels(edges: JudgeEdgeAnswer[]): Map<string, string> {
  const latest = new Map<string, JudgeEdgeAnswer>()
  for (const e of edges) {
    if (!e.rendered) continue
    const prev = latest.get(e.rendered)
    if (!prev || prev.seq < e.seq) latest.set(e.rendered, e)
  }
  return new Map([...latest].map(([k, e]) => [k, judgeEdgeText(e)]))
}

/** A judge edge's answer: `p 0.82 >= 0.70` as the server words it, or `unknown`. */
export function judgeEdgeText(e: Pick<JudgeEdgeAnswer, 'label' | 'p'>): string {
  return e.p === null || !e.label ? 'unknown' : e.label
}

/**
 * The edge looks of the run page: the edges of the graph's shown segment by
 * rendered address (the canvas draws that segment's definition). `running`
 * names the nodes running now, for the live decoration.
 */
export function runEdgeLooks(graph: RunGraph, running: ReadonlySet<string> = new Set()): Map<string, EdgeRunLook> {
  const out = new Map<string, EdgeRunLook>()
  if (graph.shown_segment === null) return out
  const judge = judgeEdgeLabels(graph.judge_edges)
  // Counts are per segment: every segment that executed the shown bytes
  // (a resume, or a patch away and back) adds its own deliveries.
  const shown = graph.segments.find((s) => s.index === graph.shown_segment)?.digest
  const same = new Set(
    graph.segments.filter((s) => s.index === graph.shown_segment || (shown && s.digest === shown)).map((s) => s.index),
  )
  for (const e of graph.edges) {
    if (!same.has(e.segment)) continue
    const look = edgeLook(e)
    const prev = out.get(e.rendered)
    out.set(e.rendered, prev ? merge(prev, look) : look)
  }
  for (const [key, look] of out) {
    const to = graph.edges.find((e) => e.rendered === key)?.to
    if (look.state === 'taken' && to && running.has(to)) look.live = true
    const j = judge.get(key)
    if (j) look.judge = `judge ${j}`
  }
  return out
}

/**
 * A banner over a run canvas, or null: the canvas draws one definition, so
 * a run that ran under more than one, or under one nobody can read, says so.
 */
export function segmentBanner(graph: RunGraph): string | null {
  const unknown = graph.segments.filter((s) => !s.known)
  // A resume over the same bytes opens a segment too: only distinct
  // definitions count.
  const distinct = new Set(graph.segments.filter((s) => s.known).map((s) => s.digest)).size + unknown.length
  if (distinct > 1) {
    return `This run ran under ${distinct} definitions (patched, or resumed under another one)${
      unknown.length ? `, ${unknown.length} of them unknown` : ''
    }; the canvas shows the latest known one and only its edge states.`
  }
  if (unknown.length) return 'The definition this run executed is unknown; edge states are not shown.'
  return null
}

/** The class of a "why it did not run" statement in one word. */
export function classWord(c: ExplainClass): string {
  return c === 'observed' ? 'seen' : c
}

/** A "why it did not run" statement as a tooltip: the summary, then each entry. */
export function notRunTitle(n: NotRun): string {
  const lines = [`not executed (${classWord(n.class)}): ${n.detail}`]
  for (const i of n.inputs) {
    const entry =
      i.entry.kind === 'edge'
        ? `edge ${i.entry.rendered} from ${i.entry.from}`
        : `policy ${i.entry.policy}${i.entry.source ? ` from ${i.entry.source}` : ''}`
    lines.push(`- ${entry}: ${classWord(i.class)}, ${i.detail}`)
  }
  return lines.join('\n')
}

/** The counts of one node over the run: `x3`, with the statuses in the tooltip. */
export function nodeCounts(n: NodeRun): { badge: string | null; title: string } {
  const parts = [`${n.executions} ${n.executions === 1 ? 'execution' : 'executions'}`]
  if (n.open) parts.push(`${n.open} open`)
  if (n.cancelled) parts.push(`${n.cancelled} cancelled by a race`)
  const by = Object.entries(n.by_status)
    .map(([s, c]) => `${s} ${c}`)
    .join(', ')
  if (by) parts.push(`ended: ${by}`)
  if (n.last_status && n.last_status !== n.status) parts.push(`latest ended ${n.last_status}`)
  return { badge: n.executions > 1 ? `x${n.executions}` : null, title: parts.join('; ') }
}

/** What a judge card shows of its latest execution. */
export interface JudgeCard {
  /** Up to three `id: value` lines with their comparison. */
  lines: string[]
  /** How many more answers there are. */
  more: number
  /** The routed values where they differ from the model's answer. */
  routed: string[]
  /** The answer was not applied: a shadow run, or a mode below enforce. */
  shadow: boolean
  title: string
}

const MAX_JUDGE_LINES = 3

function valueText(v: unknown): string {
  if (v === null || v === undefined) return 'none'
  if (typeof v === 'number') return Number.isInteger(v) ? String(v) : v.toFixed(2)
  if (typeof v === 'string') return v
  return JSON.stringify(v)
}

/** The latest execution's answers of each judge node. */
export function latestJudges(judges: JudgeNodeAnswers[]): Map<string, JudgeNodeAnswers> {
  const out = new Map<string, JudgeNodeAnswers>()
  for (const j of judges) {
    const prev = out.get(j.node)
    if (!prev || prev.execution < j.execution) out.set(j.node, j)
  }
  return out
}

/**
 * A judge node's card: each answer as `id: value` with its question's
 * comparison (`confidence 0.30 < 0.70: takes below`), the value routing
 * read apart from the model's, a shadow mark, and the native and emulation
 * evidence apart in the tooltip.
 */
export function judgeCard(j: JudgeNodeAnswers): JudgeCard {
  const all: string[] = []
  const routed: string[] = []
  for (const q of j.questions) {
    for (const a of q.answers) {
      const src = a.source === 'emulation' ? ' [emulation]' : ''
      const cmp = a.comparison ? ` (${a.comparison.label})` : a.invalid ? ` (invalid: ${a.invalid})` : ''
      all.push(`${q.id}: ${valueText(a.value)}${cmp}${src}`)
    }
    const said = q.answers.map((a) => valueText(a.value))
    if (q.routed_value !== undefined && q.routed_value !== null) {
      const r = valueText(q.routed_value)
      if (!said.includes(r) || said.length === 0) routed.push(`${q.id} = ${r}`)
    }
  }
  const shadow = j.evidence.length > 0 && j.evidence.every((e) => !e.applied || e.mode === 'shadow')
  const evidence = j.evidence.map(
    (e) =>
      `${e.source}: ${e.mode}${e.applied ? ', applied' : ', not applied'}${
        e.provider ? `, ${e.provider}${e.model ? `/${e.model}` : ''}` : ''
      }${e.error ? `, error ${e.error}` : ''}`,
  )
  const title = [
    `execution ${j.execution}${j.status ? `, ${j.status}` : ', open'}`,
    j.decided_by ? `decided by ${j.decided_by}${j.reason ? ` (${j.reason})` : ''}` : '',
    j.fallback ? `when unavailable: ${j.fallback}` : '',
    ...all,
    ...routed.map((r) => `routed ${r}`),
    ...evidence,
  ]
    .filter(Boolean)
    .join('\n')
  // No answer at all (a fallback answered): say who decided.
  if (all.length === 0 && j.decided_by) all.push(`decided by ${j.decided_by}${j.reason ? ` (${j.reason})` : ''}`)
  return {
    lines: all.slice(0, MAX_JUDGE_LINES),
    more: Math.max(0, all.length - MAX_JUDGE_LINES),
    routed,
    shadow,
    title,
  }
}

// --- the playbook page's "last run" layer ---

/** An alignment as a badge. */
export function alignmentText(a: Alignment | null | undefined): string {
  switch (a) {
    case 'exact':
      return 'exact'
    case 'same_version_other_definition':
      return 'same version, other definition'
    case 'patched':
      return 'patched'
    case 'other_version':
      return 'other version'
    default:
      return 'unknown'
  }
}

/** The tone of an alignment badge. */
export function alignmentTone(a: Alignment | null | undefined): 'ok' | 'warn' | 'muted' {
  return a === 'exact' ? 'ok' : a === 'unknown' || !a ? 'muted' : 'warn'
}

/** A candidate row of the run picker. */
export function candidateLine(c: OverlayCandidate): string {
  return `${c.run_id} · ${c.version}`
}

/** The overlay of a run projected onto the shown version. */
export interface ProjectedOverlay {
  /** Edge looks by the shown version's rendered address. */
  edges: Map<string, EdgeRunLook>
  /** Rows of segments that cannot be projected, listed apart. */
  unprojected: { segment: number; text: string }[]
  /** Nodes the run's definitions have and the shown version lacks. */
  missingInShown: string[]
}

function merge(a: EdgeRunLook, b: EdgeRunLook): EdgeRunLook {
  if (a.state === 'taken' || b.state === 'taken') {
    const count = (a.state === 'taken' ? (a.count ?? 1) : 0) + (b.state === 'taken' ? (b.count ?? 1) : 0)
    return edgeLook({ state: 'taken', count })
  }
  if (a.state === 'not_established') return a
  if (b.state === 'not_established') return b
  return a
}

function traversalText(t: TraversalRow): string {
  return `not established: ${t.from} -> ${t.to}${t.reason ? ` (${t.reason})` : ''}`
}

/**
 * Projects each edge state through its own segment's mapping
 * (`before` = the run segment's address, `after` = the shown address). An
 * unknown segment (no correspondence) is never projected: its traversals
 * are listed as not established.
 */
export function projectOverlay(overlay: Pick<PlaybookOverlay, 'mapping'>, graph: RunGraph): ProjectedOverlay {
  const bySegment = new Map<number, SegmentMapping>(overlay.mapping.map((m) => [m.segment, m]))
  const edges = new Map<string, EdgeRunLook>()
  const unprojected: ProjectedOverlay['unprojected'] = []
  const missing = new Set<string>()
  for (const m of overlay.mapping) for (const n of m.missing_in_shown) missing.add(n)
  for (const e of graph.edges) {
    const corr = bySegment.get(e.segment)?.correspondence
    if (!corr) continue
    const row = corr.find((c) => c.before && sameAddress(c.before, e.edge))
    if (!row?.after) continue
    const key = renderAddress(row.after as EdgeAddress)
    const look = edgeLook(e)
    const prev = edges.get(key)
    edges.set(key, prev ? merge(prev, look) : look)
  }
  for (const t of graph.traversals) {
    const seg = t.segment
    const known = seg !== null && !!bySegment.get(seg)?.correspondence
    if (known && t.resolved) continue
    unprojected.push({ segment: seg ?? -1, text: traversalText(t) })
  }
  return { edges, unprojected, missingInShown: [...missing] }
}

/** The banner of the "last run" layer, or null for an exact run. */
export function overlayBanner(o: Pick<PlaybookOverlay, 'alignment' | 'reason' | 'chosen'>, missingInShown: string[]): string | null {
  if (!o.chosen) return null
  const parts: string[] = []
  if (o.alignment !== 'exact') {
    parts.push(`Run ${o.chosen} is ${alignmentText(o.alignment)}${o.reason ? `: ${o.reason}` : ''}.`)
  }
  if (missingInShown.length) parts.push(`Nodes the run had and this version lacks: ${missingInShown.join(', ')}.`)
  return parts.length ? parts.join(' ') : null
}
