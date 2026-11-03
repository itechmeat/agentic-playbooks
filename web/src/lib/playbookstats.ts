import type {
  Comparison,
  DigestStats,
  NodeStats,
  PerRun,
  QualityDelta,
  QualityStats,
  Rate,
  ReasonCount,
  StatsReport,
  VersionStats,
  Waits,
} from './api.gen'
import { costByBasisText, costWithBasis } from './runusage'
import { EXPLAIN_CLASSES } from './consts.gen'
import { classWord } from './runoverlay'
import { tallyLeftOut } from './quality'

// The playbook page's stats card (C3): pure formatting of `GET /api/stats`.
// Every rate is shown with its count, because small samples mislead.

// `7/9 (78%)`, or `0/0` when nothing was counted.
export function rateText(r: Rate): string {
  return r.rate === undefined ? `${r.count}/${r.of}` : `${r.count}/${r.of} (${Math.round(r.rate * 100)}%)`
}

// `0.33 per run (1 over 3 runs)`, or `0` when there were no runs. The same
// text as `apb stats`.
export function perRunText(p: PerRun): string {
  return p.per_run === undefined ? '0' : `${p.per_run.toFixed(2)} per run (${p.total} over ${p.runs} runs)`
}

function dur(ms: number): string {
  const s = ms / 1000
  if (s < 60) return `${Math.round(s)} s`
  if (s < 3600) return `${(s / 60).toFixed(1)} min`
  return `${(s / 3600).toFixed(1)} h`
}

// `median 1.0 min over 3`, or `none`.
export function waitsText(w: Waits): string {
  return w.median_ms === undefined ? 'none' : `median ${dur(w.median_ms)} over ${w.count}`
}

// `1234 tokens per run (1234 over 1 runs), $0.0200 per run reported by 1 of
// 4 runs`, or null without reported usage. The averages cover only the runs
// that reported usage or cost, so both say how many, as `apb stats` does.
export function spendText(v: VersionStats): string | null {
  const s = v.spend
  if (s.runs_with_usage === 0) return null
  const t = s.tokens
  let line = `${(t.per_run ?? 0).toFixed(2)} tokens per run (${t.total} over ${t.runs} runs)`
  if (s.cost_per_run_usd !== undefined) {
    line += `, $${s.cost_per_run_usd.toFixed(4)} per run reported by ${s.runs_with_cost} of ${v.runs} runs`
    // What the figures are: a per-run average mixes bases, the split says
    // how much of it anybody is billed.
    const split = s.cost_by_basis ? costByBasisText(s.cost_by_basis) : ''
    line += ` (in all ${split || costWithBasis(s.cost_usd, undefined)})`
  }
  return line
}

// `3/4 (75%) attempts`: how many of the version's finished attempts
// reported a cost, so a spend figure says how much of the work it covers;
// null before any attempt finished. The same share as `apb stats`.
export function costKnownText(v: Pick<VersionStats, 'spend'>): string | null {
  const k = v.spend.cost_known
  return k.of > 0 ? `${rateText(k)} attempts` : null
}

// `glm-5.3-flash x5, opus x1 (5 differ from the profile)`: the models the
// version's attempts actually ran on (issue #193), as `apb stats` prints
// them, or null before any attempt.
export function modelsText(v: Pick<VersionStats, 'models' | 'model_mismatch'>): string | null {
  const entries = Object.entries(v.models ?? {}).sort(([a], [b]) => a.localeCompare(b))
  if (!entries.length) return null
  let line = entries.map(([m, n]) => `${m} x${n}`).join(', ')
  if (v.model_mismatch > 0) line += ` (${v.model_mismatch} differ from the profile)`
  return line
}

// `profile 1a2b3c4d5e6f (bundle 9f8e7d6c5b4a): 3 runs, first pass 2/3 (67%),
// median 1.0 min, 200 tokens per visit`: one executor group of a node, as
// `apb stats` prints it.
export function digestText(d: DigestStats): string {
  let line = `profile ${d.profile} (bundle ${d.bundle}): ${d.runs} ${d.runs === 1 ? 'run' : 'runs'}, first pass ${rateText(d.first_pass)}`
  if (d.duration.median_ms !== undefined) line += `, median ${dur(d.duration.median_ms)}`
  if (d.tokens_per_visit !== undefined) line += `, ${d.tokens_per_visit} tokens per visit`
  return line
}

// The nodes of a version that ran under more than one executor (a profile,
// its model, SOUL or skills changed between runs), each with its groups.
export function executorGroups(v: Pick<VersionStats, 'nodes'>): { node: string; groups: string[] }[] {
  return (v.nodes ?? [])
    .filter((n) => (n.digests?.length ?? 0) > 1)
    .map((n) => ({ node: n.node, groups: (n.digests ?? []).map(digestText) }))
}

// The versions newest first (the API lists them oldest first).
export function versionsNewestFirst(r: StatsReport | null): VersionStats[] {
  return r ? [...r.versions].reverse() : []
}

type QualityKey = Pick<QualityStats, 'criterion' | 'grader' | 'task_type' | 'digest'>

// `outcome (rules/1, default)`: the comparable key of a quality row, as
// `apb stats` names it.
export function qualityLabel(q: Omit<QualityKey, 'digest'>): string {
  return `${q.criterion} (${q.grader}, ${q.task_type})`
}

// A key unique within one list: rows of one criterion graded under two
// definitions (two digests) differ only by the digest.
export function qualityKey(q: QualityKey): string {
  return `${q.criterion} ${q.grader} ${q.task_type} ${q.digest}`
}

// The labels of `rows`, each with its definition's digest
// (`outcome (rules/1, default), definition 1a2b3c4d`) when another row
// shares the rest of its key, so the rows can be told apart.
export function qualityLabels(rows: QualityKey[]): string[] {
  return rows.map((q) => {
    const twin = rows.some((o) => o !== q && qualityLabel(o) === qualityLabel(q))
    if (!twin) return qualityLabel(q)
    return `${qualityLabel(q)}, definition ${q.digest.replace(/^sha256:/, '').slice(0, 8)}`
  })
}

// `good 2/3 (67%), poor 1/3 (33%) over 3 graded runs, 1 not applicable`:
// one criterion over a version's graded runs, the same text as `apb stats`
// plus what the shares leave out.
export function qualityText(q: QualityStats): string {
  let line = `good ${rateText(q.good)}, poor ${rateText(q.poor)} over ${q.graded_runs} graded ${q.graded_runs === 1 ? 'run' : 'runs'}`
  const left = tallyLeftOut(q.tally)
  if (left) line += `, ${left}`
  return line
}

// The sign follows the rounded value, so a tiny drop never reads `0` next
// to a tiny rise reading `+0`.
function pointsText(d: number | undefined): string {
  if (d === undefined) return 'n/a'
  const p = Math.round(d * 100) || 0
  return `${p >= 0 ? '+' : ''}${p} points`
}

// `good +17 points, poor -17 points`: how one criterion moved between the compared
// versions.
export function qualityDeltaText(d: QualityDelta): string {
  return `good ${pointsText(d.good_delta)}, poor ${pointsText(d.poor_delta)}`
}

// The version a stats card compares the newest one with: the one before
// it, when there is one (the API lists versions oldest first).
export function previousVersion(r: StatsReport | null): string {
  const v = r?.versions ?? []
  return v.length >= 2 ? v[v.length - 2].version : ''
}

// The quality deltas of a comparison that moved by a number: a criterion
// neither version could grade to a level (all not applicable) is left out.
export function qualityDeltas(c: Comparison | null | undefined): QualityDelta[] {
  return (c?.quality ?? []).filter((d) => d.good_delta !== undefined || d.poor_delta !== undefined)
}

// --- per node: executions and why a node did not run ---

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`

// `seen 2, inferred 1, unknown 1`: the runs a node did not run in, by the
// class of the explanation, strongest first.
export function reasonClassText(reasons: ReasonCount[]): string {
  return EXPLAIN_CLASSES.map((c) => [c, reasons.filter((r) => r.class === c).reduce((n, r) => n + r.runs, 0)] as const)
    .filter(([, n]) => n > 0)
    .map(([c, n]) => `${classWord(c)} ${n}`)
    .join(', ')
}

// `routed elsewhere: 2 (seen)` per reason, for a tooltip or a list.
export function reasonLines(reasons: ReasonCount[]): string[] {
  return reasons.map((r) => `${r.code.replace(/_/g, ' ')}: ${r.runs} (${classWord(r.class)})`)
}

// One node's row: executions, completed, open, failed, cancelled race
// losers apart, and the runs it belongs to. A node no run executed has a
// row of zeros.
export function nodeStatsText(n: NodeStats): string {
  const parts = [
    plural(n.executions, 'execution'),
    `${n.completed} completed`,
    `${n.open} open`,
    `${n.failed} failed`,
  ]
  if (n.cancelled) parts.push(`${n.cancelled} cancelled by a race`)
  let line = `${parts.join(', ')}; in ${n.runs} of ${plural(n.in_runs, 'run')}`
  if (n.in_runs_unknown) line += `, membership unknown in ${plural(n.in_runs_unknown, 'run')}`
  return line
}

// The counts layer of one node: `executed 3 times in 2 of 4 runs`, then the
// runs it did not run in with their reasons by class. Never a verdict on
// the branch: an `unknown` reason stays unknown.
export function nodeLayer(n: NodeStats): { text: string; title: string } {
  const times = n.executions === 1 ? 'once' : `${n.executions} times`
  let text = n.executions ? `executed ${times} in ${n.runs} of ${plural(n.in_runs, 'run')}` : `not executed in ${plural(n.in_runs, 'run')}`
  const missed = Math.max(0, n.in_runs - n.runs)
  const classes = reasonClassText(n.not_run)
  if (n.executions && missed) text += `; not executed in ${plural(missed, 'run')}`
  if (classes) text += ` (${classes})`
  if (n.in_runs_unknown) text += `; membership unknown in ${plural(n.in_runs_unknown, 'run')}`
  const title = [
    nodeStatsText(n),
    ...reasonLines(n.not_run),
    n.in_runs_unknown ? `membership unknown in ${plural(n.in_runs_unknown, 'run')}` : '',
  ]
    .filter(Boolean)
    .join('\n')
  return { text, title }
}

// The node rows of the version a page shows, by node id.
export function nodeStatsOf(r: StatsReport | null, version: string): Map<string, NodeStats> {
  const v = r?.versions.find((x) => x.version === version)
  return new Map((v?.nodes ?? []).map((n) => [n.node, n]))
}
