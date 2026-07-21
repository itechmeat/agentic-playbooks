import type { PendingQuestion, WfEvent } from './types'

// A pending question surfaced to the question panel: the node, the question
// text, and any suggested options. Deliberately narrower than the server's
// `PendingQuestion` (which also carries `answer_by`/`asked_at`) since the
// panel only ever posts as "human" and does not display those fields.
export interface QuestionEntry {
  node: string
  question: string
  options: string[]
}

// interactive agent_task nodes awaiting an answer: a question_asked event
// without a subsequent question_answered (by comparing event counts of each
// kind), mirroring reviews.ts's pendingReviews exactly.
export function pendingQuestions(events: WfEvent[]): QuestionEntry[] {
  const asked = new Map<string, { count: number; question: string; options: string[] }>()
  const answered = new Map<string, number>()
  for (const e of events) {
    if (e.type === 'question_asked' && e.node) {
      const prev = asked.get(e.node)
      const question = typeof e.question === 'string' ? e.question : (prev?.question ?? '')
      const raw = e.options
      const options = Array.isArray(raw) ? (raw as string[]) : (prev?.options ?? [])
      asked.set(e.node, { count: (prev?.count ?? 0) + 1, question, options })
    } else if (e.type === 'question_answered' && e.node) {
      answered.set(e.node, (answered.get(e.node) ?? 0) + 1)
    }
  }
  const out: QuestionEntry[] = []
  for (const [node, { count, question, options }] of asked) {
    if (count > (answered.get(node) ?? 0)) {
      out.push({ node, question, options })
    }
  }
  return out
}

// Fallback for the window before drive journals `question_asked`: the run
// payload's `progress.pending_question` is read directly from the
// questions.jsonl/answers.jsonl channel files (spec 2026-07-20-interactive-nodes,
// `progress::pending_question_for_run`), so it is visible before the event
// log carries anything at all. Used only when `pendingQuestions(events)`
// finds nothing, so a run whose question IS already journaled keeps using the
// event-derived (possibly multi-entry) list.
export function pendingQuestionsFromPayload(
  pending: PendingQuestion | null | undefined,
): QuestionEntry[] {
  if (!pending) return []
  return [{ node: pending.node, question: pending.question, options: pending.options }]
}
