import type {
  Evidence,
  EvidenceBasis,
  EvidenceSource,
  FollowUpResultView,
  FreshnessState,
  Gap,
  GapKind,
  NeedsLook,
  RunFinding,
  RunFollowUp,
  RunWarning,
} from './api.gen'
import { EVIDENCE_BASIS_LABELS, EVIDENCE_SOURCE_LABELS, FRESHNESS_LABELS, GAP_LABELS } from './consts.gen'
import { when } from './pause'

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
const basisLabels: Readonly<Record<EvidenceBasis, string>> = EVIDENCE_BASIS_LABELS
const freshnessLabels: Readonly<Record<FreshnessState, string>> = FRESHNESS_LABELS

// `goal`, `success check`, `baseline`.
export function evidenceSource(e: Evidence): string {
  return evidenceSourceLabels[e.source] ?? e.source
}

// A goal criterion that passed or a baseline rule that was met, and still
// describes the tree the run left; a pass that is stale or not known to be
// current is shown as a gap, never as a pass. A failed check stays failed,
// whatever its freshness.
export function evidenceTone(e: Evidence): Tone {
  if (e.result !== 'passed' && e.result !== 'met') return 'fail'
  return evidenceIsCurrent(e) ? 'pass' : 'gap'
}

// Whether nothing says the result is out of date (`Evidence::is_current`).
export function evidenceIsCurrent(e: Evidence): boolean {
  return !e.freshness || e.freshness.state === 'current'
}

// `executed`, `agent's own text`, `model judgment`, `read by apb`.
export function evidenceBasis(e: Evidence): string {
  return basisLabels[e.basis] ?? e.basis
}

// `stale: a file in its scope (src/**) changed after the check`, or null
// for a result that is current or never bound.
export function freshnessText(e: Evidence): string | null {
  if (!e.freshness || e.freshness.state === 'current') return null
  const label = freshnessLabels[e.freshness.state] ?? e.freshness.state
  return e.freshness.reason ? `${label}: ${e.freshness.reason}` : label
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
  return (
    g.kind === 'inbox_gap' ||
    g.kind === 'unknown_call' ||
    g.kind === 'incomplete_response' ||
    g.kind === 'item_needs_look'
  )
}

// `1 regression, 2 findings, 1 warning, 1 unknown send, 2 list items`, the same text as
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
  add(n.list_items ?? 0, 'list item', 'list items')
  add(n.green_checks ?? 0, 'check green before work', 'checks green before work')
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

// The follow-up block's status line: the status word with the time that
// matters (`due <utc>` while pending) and, once observed, the verdict
// (`met`, not `not_met`) and the read time (`read <utc>`). Every other
// status shows as the bare word; its reason line carries why.
export function followUpStatusLine(f: RunFollowUp): string {
  const parts = [f.status]
  if (f.status === 'observed') {
    if (f.verdict) parts.push(resultText(f.verdict))
    if (f.observed_at_ms !== undefined) parts.push(`read ${when(f.observed_at_ms)}`)
  }
  if (f.status === 'pending' && f.due_at_ms !== undefined) parts.push(`due ${when(f.due_at_ms)}`)
  return parts.length === 1 ? parts[0] : `${parts[0]}: ${parts.slice(1).join(', ')}`
}

// One result of the follow-up read: `equals won on stage (won)`, bare
// without a detail.
export function followUpResultLine(r: FollowUpResultView): string {
  const detail = r.detail ? ` (${r.detail})` : ''
  return `${r.rule} on ${r.observation}${detail}`
}
