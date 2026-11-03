import type { RunDecisions } from './api.gen'
import { formatCost } from './runusage'
import { shownNumber } from './format'
import type { WfEvent } from './types'

// Decision-model totals and rows on the run page (issue #165 Part 4). The
// totals come from the server's `RunDetail.decisions`; each row is read from
// a `decision_made` event, whose shape a newer apb may extend, so every field
// is checked before use.

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`
}

// The totals line: `3 decisions (1 replayed, 1 error) · $0.0004 · p50 190 ms
// · p95 250 ms · shadow would change 2`.
export function decisionsSummary(d: RunDecisions): string {
  const extra: string[] = []
  if (d.replayed > 0) extra.push(`${d.replayed} replayed`)
  if (d.errors > 0) extra.push(plural(d.errors, 'error', 'errors'))
  const parts = [plural(d.decisions, 'decision', 'decisions') + (extra.length ? ` (${extra.join(', ')})` : '')]
  parts.push(`${formatCost(d.cost_usd)}${d.cost_estimated ? ' estimated' : ''}`)
  if (d.p50_latency_ms !== undefined) parts.push(`p50 ${d.p50_latency_ms} ms`)
  if (d.p95_latency_ms !== undefined) parts.push(`p95 ${d.p95_latency_ms} ms`)
  const would = Object.values(d.by_use).reduce((n, u) => n + u.shadow_would_change, 0)
  if (would > 0) parts.push(`shadow would change ${would}`)
  return parts.join(' · ')
}

export interface DecisionUseLine {
  use: string
  text: string
}

// One line per use: `2 requests · 1 error · 0 applied · 1 would change`.
export function decisionUseLines(d: RunDecisions): DecisionUseLine[] {
  return Object.entries(d.by_use).map(([use, u]) => {
    const parts = [plural(u.requests, 'request', 'requests')]
    if (u.errors > 0) parts.push(plural(u.errors, 'error', 'errors'))
    parts.push(`${u.applied} applied`)
    if (u.shadow_would_change > 0) parts.push(`${u.shadow_would_change} would change`)
    return { use, text: parts.join(' · ') }
  })
}

export interface DecisionRow {
  seq: number
  use: string
  node: string | null
  /** `final_result p=0.91, completion complete (p=0.83)`, or the error. */
  answer: string
  /** `main/jev-1.13.0`, or the provider alone when no model answered. */
  source: string
  latency: string
  /** `applied`, `shadow`, `shadow, would change`, the mode, or `error`. */
  outcome: string
}

function num(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined
}

function str(v: unknown): string | undefined {
  return typeof v === 'string' ? v : undefined
}

function fmtP(p: number): string {
  return p.toFixed(2)
}

// The compact answers of one decision, in question order.
function answerText(answers: unknown): string {
  if (!answers || typeof answers !== 'object') return ''
  const parts: string[] = []
  for (const [q, raw] of Object.entries(answers as Record<string, unknown>)) {
    if (!raw || typeof raw !== 'object') continue
    const a = raw as Record<string, unknown>
    const p = num(a.p)
    const confidence = num(a.confidence)
    const value = a.value
    const shown = typeof value === 'number' ? shownNumber(value) : String(value)
    const how =
      p !== undefined ? ` (p=${fmtP(p)})` : confidence !== undefined ? ` (confidence ${shownNumber(confidence)})` : ''
    if (typeof a.invalid === 'string') parts.push(`${q} invalid`)
    else if (value !== undefined && value !== null) parts.push(`${q} ${shown}${how}`)
    else if (p !== undefined) parts.push(`${q} p=${fmtP(p)}`)
  }
  return parts.join(', ')
}

// One row per `decision_made`, in journal order.
export function decisionRows(events: WfEvent[]): DecisionRow[] {
  return events
    .filter((e) => e.type === 'decision_made')
    .map((e) => {
      const r = e as unknown as Record<string, unknown>
      const error = str(r.error)
      const provider = str(r.provider) ?? 'no provider'
      const model = str(r.model)
      const mode = str(r.mode) ?? ''
      const latency = num(r.latency_ms)
      let outcome: string
      if (error) outcome = 'error'
      else if (r.applied === true) outcome = 'applied'
      else if (mode === 'shadow') outcome = r.would_change === true ? 'shadow, would change' : 'shadow'
      else outcome = mode || 'not applied'
      return {
        seq: e.seq,
        use: str(r.use_site) ?? 'decision',
        node: e.node ?? null,
        answer: error ? `error: ${error}` : answerText(r.answers),
        source: model ? `${provider}/${model}` : provider,
        latency: r.cached === true ? 'cached' : latency !== undefined ? `${latency} ms` : '',
        outcome,
      }
    })
}

// The event journal note of one `decision_made`.
export function decisionNote(e: WfEvent): string {
  const [row] = decisionRows([e])
  return [row.use, row.answer, row.source, row.latency, row.outcome].filter(Boolean).join(' · ')
}

/**
 * What saving a judge node's decision as a labelled example starts from:
 * the node, its execution (1-based, counted as the engine counts them:
 * one more than the node's finished executions before the decision; absent
 * when the journal shows no start, which the server reads as the latest), and one expected answer per question the
 * decision answered, a `noul` (answered with `p` only) as yes or no, any
 * other prefilled with the observed value. Null for anything but a
 * `judge_node` decision with answers.
 */
export interface ExampleDraft {
  node: string
  execution?: number
  questions: { name: string; yesNo: boolean; observed: string }[]
}

export function exampleDraftOf(events: WfEvent[], seq: number): ExampleDraft | null {
  const e = events.find((x) => x.seq === seq && x.type === 'decision_made')
  if (!e || !e.node) return null
  const r = e as unknown as Record<string, unknown>
  if (r.use_site !== 'judge_node' || str(r.error)) return null
  const answers = r.answers && typeof r.answers === 'object' ? (r.answers as Record<string, unknown>) : {}
  const questions = Object.entries(answers)
    .filter(([, a]) => a && typeof a === 'object')
    .map(([name, raw]) => {
      const a = raw as Record<string, unknown>
      const has = a.value !== undefined && a.value !== null
      // A score's value is its expected level, a number; the example names
      // a level or a band, which the person types.
      const observed = typeof a.value === 'string' ? a.value : ''
      return { name, yesNo: !has && num(a.p) !== undefined, observed }
    })
  if (!questions.length) return null
  // Executions as `apb_engine::identity::index` numbers them: the one a
  // decision sits in follows every finished one of its node.
  const started = events.some((x) => x.type === 'node_started' && x.node === e.node && x.seq < seq)
  const finished = events.filter((x) => x.type === 'node_finished' && x.node === e.node && x.seq < seq).length
  return { node: e.node, ...(started ? { execution: finished + 1 } : {}), questions }
}
