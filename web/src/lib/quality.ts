// Run quality on the dashboard: how a grade, a level, a citation and a
// candidate comparison read on the run page (QualityPanel) and the playbook
// page (CandidateQualityCard). Pure functions over the generated types.

import type {
  Cite,
  CriterionComparison,
  CriterionResult,
  Grade,
  Level,
  ReadTrace,
  Tally,
  Verdict,
} from './api.gen'
import type { Tone } from './runverdict'
import type { WfEvent } from './types'

/** Whether one of `grades` is a host's: a person may let a host grade again. */
export function hasHostGrade(grades: Grade[]): boolean {
  return grades.some((g) => g.grader.kind === 'host')
}

/** The tag colour of a level: good passes, poor fails, the rest are gaps. */
export function levelTone(level: Level): Tone {
  switch (level) {
    case 'good':
      return 'pass'
    case 'poor':
      return 'fail'
    case 'partial':
    case 'not_enough_data':
      return 'gap'
    default:
      return 'neutral'
  }
}

/** A level as a person reads it. */
export function levelText(level: Level): string {
  return level.replace(/_/g, ' ')
}

/** `rules/1`, `host/1`, `model/1`. */
export function graderName(g: Grade): string {
  return `${g.grader.kind}/${g.grader.version}`
}

/** Who graded, as a person reads it: rules, host, decision model. */
export function graderLabel(g: Grade): string {
  return g.grader.kind === 'model' ? 'decision model' : g.grader.kind
}

/** Why a model grade stored nothing, in words. */
export function gradeSkippedText(reason: string): string {
  switch (reason) {
    case 'off':
      return 'the decision model is off for grading in this project (uses.grade in decisions.yaml, run outputs allowed to be sent, and a provider available)'
    case 'no_criteria':
      return 'the playbook has no criteria of its own'
    case 'budget':
      return "today's cap of model grades is spent"
    case 'send':
      return 'privacy.send in decisions.yaml does not allow sending prompts to the decision model'
    case 'busy':
      return 'the decision model is busy; try again shortly'
    case 'state_too_large':
      return 'the run is too large to send whole'
    default:
      return reason.replace(/_/g, ' ')
  }
}

/**
 * The latest grade of each grader, newest first. The API lists grades
 * oldest first (by time, then id); on a tie the one listed later wins, the
 * same grade a candidate comparison counts.
 */
export function latestGrades(grades: Grade[]): Grade[] {
  const latest = new Map<string, Grade>()
  for (const g of grades) {
    const key = graderName(g)
    const seen = latest.get(key)
    if (!seen || g.created_at >= seen.created_at) latest.set(key, g)
  }
  return [...latest.values()].sort((a, b) => b.created_at - a.created_at)
}

/** One line of what a grader read: characters, steps, views read to the end. */
export function readingLine(r: ReadTrace): string {
  const steps = r.ops?.length ?? 0
  const views = r.views ?? []
  const ended = views.filter((v) => v.ended).length
  const parts = [`read ${r.read_chars.toLocaleString('en-US')} of ${r.budget_chars.toLocaleString('en-US')} chars`]
  parts.push(`${steps} ${steps === 1 ? 'step' : 'steps'}`)
  if (views.length) parts.push(`${ended} of ${views.length} views to the end`)
  return parts.join(', ')
}

/** Where a citation points on the run page. */
export interface CiteTarget {
  /** What the cite names, for its link text. */
  label: string
  /** The journal event to show, when one stands for the record. */
  seq: number | null
  /** The file under the run directory that holds the record, if any. */
  path: string | null
}

const STREAM_FILES: Record<string, string> = {
  session: 'session.jsonl',
  stdout: 'stdout.log',
  stderr: 'stderr.log',
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/** The seq of the last event matching `pred`, or null. */
function lastSeq(events: WfEvent[], pred: (e: WfEvent) => boolean): number | null {
  for (let i = events.length - 1; i >= 0; i--) {
    if (pred(events[i])) return events[i].seq
  }
  return null
}

/**
 * Where citation `c` points: a journal event (`j:<seq>`), a node's output
 * (`o:<node>`, its last `node_finished`), an attempt's status file
 * (`s:<node>-<attempt>`, its `attempt_finished`) or a transcript record
 * (`t:<stem>/<stream>#<record>`, the `attempt_started` that names the
 * transcript, and the file that holds it). The dashboard never shows a
 * transcript's text: a grade cites it, a person opens the file.
 */
export function citeTarget(c: Cite, events: WfEvent[]): CiteTarget {
  const ref = c.ref
  const span = c.span ? ` chars ${c.span[0]}-${c.span[1]}` : ''
  const body = ref.slice(2)
  switch (ref.slice(0, 2)) {
    case 'j:': {
      // Plain decimal digits only: `j:`, ` 3` or `1e1` name no event.
      const seq = /^\d+$/.test(body) ? Number(body) : null
      return { label: `journal #${body}${span}`, seq, path: null }
    }
    case 'o:':
      return {
        label: `output of ${body}${span}`,
        seq: lastSeq(events, (e) => e.type === 'node_finished' && e.node === body),
        path: null,
      }
    case 's:':
      return {
        label: `status file ${body}${span}`,
        seq: lastSeq(events, (e) => e.type === 'attempt_finished' && `${str(e.node)}-${String(e.attempt)}` === body),
        path: `agent-status/${body}.json`,
      }
    case 't:': {
      const [file, record = ''] = body.split('#')
      const [stem, stream = ''] = file.split('/')
      const dir = `attempts/${stem}`
      return {
        label: `transcript ${stem} ${stream} record ${record}${span}`,
        seq: lastSeq(events, (e) => e.type === 'attempt_started' && e.transcript === dir),
        path: STREAM_FILES[stream] ? `${dir}/${STREAM_FILES[stream]}` : null,
      }
    }
    default:
      return { label: ref, seq: null, path: null }
  }
}

/** A comparison verdict as a person reads it. */
export function verdictText(v: Verdict): string {
  switch (v) {
    case 'promote':
      return 'promote'
    case 'keep_current':
      return 'keep current'
    case 'wait':
      return 'waiting for more trials'
    case 'needs_person':
      return 'needs a person'
  }
}

/** The tag colour of a verdict. */
export function verdictTone(v: Verdict): Tone {
  if (v === 'promote') return 'pass'
  if (v === 'keep_current') return 'fail'
  return 'gap'
}

/** The tag colour of one criterion's comparison. */
export function resultTone(r: CriterionResult): Tone {
  if (r === 'better') return 'pass'
  if (r === 'worse') return 'fail'
  if (r === 'insufficient' || r === 'inconclusive') return 'gap'
  return 'neutral'
}

/** Every grade a tally holds, determinate or not. */
export function tallyTotal(t: Tally): number {
  return t.good + t.partial + t.poor + t.not_applicable + t.not_enough_data
}

/**
 * `good/partial/poor`, the counts a comparison rests on; `empty` for a
 * side that has no grade at all (never `0/0/0`, which reads as graded runs
 * that all went nowhere).
 */
export function tallyText(t: Tally, empty = 'no graded runs yet'): string {
  return tallyTotal(t) === 0 ? empty : `${t.good}/${t.partial}/${t.poor}`
}

/** What a tally leaves out of the comparison, or null when nothing. */
export function tallyLeftOut(t: Tally): string | null {
  const parts: string[] = []
  if (t.not_applicable) parts.push(`${t.not_applicable} not applicable`)
  if (t.not_enough_data) parts.push(`${t.not_enough_data} not enough data`)
  return parts.length ? parts.join(', ') : null
}

/** A comparison row's criterion and the grader it compares. */
export function comparisonLabel(c: CriterionComparison): string {
  return `${c.key.criterion} (${c.key.grader})`
}
