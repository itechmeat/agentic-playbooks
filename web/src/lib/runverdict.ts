import type { Evidence, EvidenceSource, Gap, GapKind, NeedsLook, RunFinding, RunWarning } from './api.gen'
import { EVIDENCE_SOURCE_LABELS, GAP_LABELS } from './consts.gen'

// The run page's verdict panel: pure formatting of the run's human outcome
// (`RunDetail.outcome`). The panel keeps what the engine checked apart from
// what nobody checked; these helpers only word it.

// The colour a tag is shown in: a check that held, one that did not, and
// anything nobody checked or a person still has to look at.
export type Tone = 'pass' | 'fail' | 'gap' | 'neutral'

// The engine's words for each kind (`EvidenceSource::label` and
// `GapKind::label`, exported to consts.gen.ts). Typed by the generated
// enums: a kind without a label fails type-checking.
const evidenceSourceLabels: Readonly<Record<EvidenceSource, string>> = EVIDENCE_SOURCE_LABELS
const gapLabels: Readonly<Record<GapKind, string>> = GAP_LABELS

// `goal`, `success check`, `baseline`.
export function evidenceSource(e: Evidence): string {
  return evidenceSourceLabels[e.source] ?? e.source
}

// A goal criterion that passed or a baseline rule that was met.
export function evidenceTone(e: Evidence): Tone {
  return e.result === 'passed' || e.result === 'met' ? 'pass' : 'fail'
}

// `not met`, not `not_met`.
export function resultText(result: string): string {
  return result.replace(/_/g, ' ')
}

// Why something is not verified, in the engine's words.
export function gapLabel(g: Gap): string {
  return gapLabels[g.kind] ?? g.kind
}

// The gaps that mean the run saw or did less than it reports: shown as a
// warning rather than a plain note.
export function gapIsWarning(g: Gap): boolean {
  return g.kind === 'inbox_gap' || g.kind === 'unknown_call' || g.kind === 'incomplete_response'
}

// `1 regression, 2 findings, 1 warning, 1 unknown send`, the same text as
// `apb runs`, or null when nothing calls for a look.
export function needsLookCounts(n: NeedsLook | null | undefined): string | null {
  if (!n) return null
  const parts: string[] = []
  const add = (count: number, one: string, many: string) => {
    if (count > 0) parts.push(`${count} ${count === 1 ? one : many}`)
  }
  add(n.regressions, 'regression', 'regressions')
  add(n.findings, 'finding', 'findings')
  add(n.warnings, 'warning', 'warnings')
  add(n.unknown_sends ?? 0, 'unknown send', 'unknown sends')
  return parts.length ? parts.join(', ') : null
}

// `reported by fix attempt 2`.
export function reportedBy(n: RunFinding | RunWarning): string {
  return `reported by ${n.node} attempt ${n.attempt}`
}

// `yes`, `no`, or `not known` when the goal criteria cannot say.
export function goalReachedText(reached: boolean | undefined): string {
  if (reached === true) return 'yes'
  if (reached === false) return 'no'
  return 'not known'
}
