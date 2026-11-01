import { pendingSupervisorFromPayload, type SupervisorEntry } from './supervisors'
import type { PublishGate } from './publish'
import type { ActionCode, NextAction, PendingHostTask, ReviewRecommendation } from './api.gen'
import type { RunDetail } from './types'

// The identity of the request a gate entry posts to: the run it belongs to
// and the journal seq of the request, from the run's matching next action.
// A post naming it is refused once the request was replaced or settled.
export interface GateRequest {
  runId: string
  /** Absent on a payload from an older server without next actions. */
  request?: number
}

// A pending human-review gate as the review panel renders it: one button per
// option (a gate without options already carries the approve/reject
// defaults), and the gate's optional guidance above them.
export interface ReviewEntry extends GateRequest {
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
export interface QuestionEntry extends GateRequest {
  node: string
  question: string
  options: string[]
}

// A host task as the host task panel renders it (host execution mode,
// 0.23.0): read-only, the host session submits it, never the page.
export interface HostTaskEntry {
  runId: string
  taskId: string
  node: string
  attempt: number
  prompt: string
  rolePrompt: string | null
  modelHint: string | null
  /** The hint with its label (`advisory: model hint M (fallback 1 of 1
   * declared by profile P ...)`, or the primary executor), from the server. */
  hintNote: string | null
  /** How the host executes the task: the engine's execution contract,
   * verbatim from the server (never worded on this side). */
  executionNote: string
  /** Milliseconds since epoch by which the host must submit, or null. */
  deadline: number | null
}

// Every gate the run page renders a panel for. A review that carries a
// publish plan is a publish gate: it goes to the publish panel, which shows
// the exact plan and posts the plan's nonce, never to the review card.
export interface RunGates {
  reviews: ReviewEntry[]
  publishes: (PublishGate & GateRequest)[]
  questions: QuestionEntry[]
  waits: string[]
  supervisor: SupervisorEntry | null
  tasks: HostTaskEntry[]
}

export function hostTaskEntry(t: PendingHostTask): HostTaskEntry {
  return {
    runId: t.run_id,
    taskId: t.task_id,
    node: t.node,
    attempt: t.attempt,
    prompt: t.prompt,
    rolePrompt: t.role_prompt,
    modelHint: t.model_hint,
    hintNote: t.hint_note ?? null,
    executionNote: t.execution_note,
    deadline: t.deadline,
  }
}

// `in 42s`, `in 3m`, `overdue`: how long the host has left for a task.
export function deadlineNote(deadline: number | null, now: number): string | null {
  if (deadline === null) return null
  const left = Math.round((deadline - now) / 1000)
  if (left <= 0) return 'overdue'
  if (left < 120) return `in ${left}s`
  if (left < 7200) return `in ${Math.round(left / 60)}m`
  return `in ${Math.round(left / 3600)}h`
}

// The run page's gates, read from the server's derived progress: the same
// RunView `apb wait`, `apb runs` and MCP run_status/run_wait report from.
// The page never re-derives them from the event log, where it used to count
// events on its own and could disagree with the engine (a wait shown as
// "awaiting signal" on a run that had already stopped).
export function runGates(detail: RunDetail): RunGates {
  const p = detail.progress
  if (!p) return { reviews: [], publishes: [], questions: [], waits: [], supervisor: null, tasks: [] }
  const req = (code: ActionCode, node: string) => gateRequest(detail, code, node)
  return {
    reviews: p.pending_reviews
      .filter((r) => !r.publish)
      .map(({ node, options, prompt, recommendation }) => {
        const entry: ReviewEntry = { node, options, ...(prompt ? { prompt } : {}), ...req('decide_review', node) }
        const note = recommendationNote(recommendation)
        if (note) entry.recommendation = note
        return entry
      }),
    publishes: p.pending_reviews.flatMap(({ node, publish, prompt }) =>
      publish
        ? [{ node, plan: publish, ...(prompt ? { prompt } : {}), ...req('decide_publish', node) }]
        : [],
    ),
    questions: p.pending_questions.map(({ node, question, options }) => ({
      node,
      question,
      options,
      ...req('answer_question', node),
    })),
    waits: p.pending_waits,
    supervisor: pendingSupervisorFromPayload(p.pending_supervisor),
    // Absent on a payload from an older server.
    tasks: (p.pending_tasks ?? []).map(hostTaskEntry),
  }
}

// The request of the page's own run that a gate of `node` answers: the run
// id, and the seq of the matching next action (same code, node and run).
export function gateRequest(detail: RunDetail, code: ActionCode, node: string): GateRequest {
  const actions: NextAction[] = detail.next_actions?.actions ?? []
  const a = actions.find((x) => x.code === code && x.target.node === node && x.run_id === detail.run_id)
  const seq = a?.target.request?.seq
  return seq === undefined ? { runId: detail.run_id } : { runId: detail.run_id, request: seq }
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
