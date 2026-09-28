import { pendingSupervisorFromPayload, type SupervisorEntry } from './supervisors'
import type { ReviewRecommendation } from './api.gen'
import type { RunDetail } from './types'

// A pending human-review gate as the review panel renders it: one button per
// option (a gate without options already carries the approve/reject
// defaults), and the gate's optional guidance above them.
export interface ReviewEntry {
  node: string
  options: string[]
  prompt?: string
  /** The decision model's advisory recommendation, as one line (issue #165
   * Part 11); absent unless the machine runs `review_triage` in advise or
   * enforce. Never preselects an option. */
  recommendation?: string
}

// A pending question as the question panel renders it. The panel always
// answers as "human", so the server's answer_by/asked_at are not carried.
export interface QuestionEntry {
  node: string
  question: string
  options: string[]
}

// Every gate the run page renders a panel for.
export interface RunGates {
  reviews: ReviewEntry[]
  questions: QuestionEntry[]
  waits: string[]
  supervisor: SupervisorEntry | null
}

// The run page's gates, read from the server's derived progress: the same
// RunView `apb wait`, `apb runs` and MCP run_status/run_wait report from.
// The page never re-derives them from the event log, where it used to count
// events on its own and could disagree with the engine (a wait shown as
// "awaiting signal" on a run that had already stopped).
export function runGates(detail: RunDetail): RunGates {
  const p = detail.progress
  if (!p) return { reviews: [], questions: [], waits: [], supervisor: null }
  return {
    reviews: p.pending_reviews.map(({ node, options, prompt, recommendation }) => {
      const entry: ReviewEntry = prompt ? { node, options, prompt } : { node, options }
      const note = recommendationNote(recommendation)
      if (note) entry.recommendation = note
      return entry
    }),
    questions: p.pending_questions.map(({ node, question, options }) => ({
      node,
      question,
      options,
    })),
    waits: p.pending_waits,
    supervisor: pendingSupervisorFromPayload(p.pending_supervisor),
  }
}

// The review card's recommendation line: `Advisory recommendation: approve
// (p=0.86), provider/model`, marked when uncalibrated or already applied by
// the engine. Nothing without a recommendation.
export function recommendationNote(r: ReviewRecommendation | null | undefined): string | undefined {
  if (!r || !r.option) return undefined
  const who = [r.provider, r.model].filter(Boolean).join('/')
  const flags = [r.calibrated ? null : 'uncalibrated', r.applied ? 'applied by the engine' : null].filter(Boolean)
  return `${r.applied ? 'Decided' : 'Advisory recommendation'}: ${r.option} (p=${r.p.toFixed(2)})${who ? `, ${who}` : ''}${flags.length ? ` [${flags.join(', ')}]` : ''}`
}
