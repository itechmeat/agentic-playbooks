import { describe, expect, it } from 'vitest'
import { deadlineNote, runGates } from './rungates'
import type { ProgressSummary, RunDetail, WfEvent } from './types'

const progress = (over: Partial<ProgressSummary> = {}): ProgressSummary => ({
  percent: 0,
  label: null,
  waiting_on: null,
  waiting_kind: null,
  pending_question: null,
  pending_review: null,
  pending_supervisor: null,
  pending_reviews: [],
  pending_questions: [],
  pending_waits: [],
  pending_tasks: [],
  plan_key: '1.0.0|',
  ...over,
})

const detail = (events: Partial<WfEvent>[], p: ProgressSummary, status: RunDetail['run_status']): RunDetail => ({
  run_id: 'r',
  playbook: 'p',
  version: '1.0.0',
  run_status: status,
  failure_reason: null,
  driver_alive: null,
  nodes: {},
  outputs: {},
  instruction: null,
  params: {},
  worktree: null,
  model: null,
  layout: null,
  hooks: {},
  children: [],
  progress: p,
  answer: null,
  usage: null,
  unknown_events: 0,
  execution: 'cli',
  execution_fallback: false,
  events: events.map((e, i) => ({ seq: i, ts: i, type: '', ...e })),
})

// F16: the run page shows exactly the gates the server derived (the same
// RunView `apb wait` and MCP run_wait decide on), never a second derivation
// from the raw event log that can disagree with them.
describe('runGates', () => {
  it('shows no wait on a run the server reports as not waiting', () => {
    // An aborted run whose journal still has an unmatched wait_started: the
    // engine does not count it as waiting, so neither may the page.
    const d = detail(
      [
        { type: 'run_started' },
        { type: 'node_started', node: 'w' },
        { type: 'wait_started', node: 'w' },
        { type: 'run_aborted' },
      ],
      progress(),
      'aborted',
    )
    expect(runGates(d).waits).toEqual([])
  })

  it('shows every gate the server lists, with its options', () => {
    const review = {
      node: 'gate',
      instruction: 'decide',
      options: ['approve', 'reject'],
      how_to_decide: 'apb review',
    }
    const question = {
      node: 'ask',
      question: 'Which?',
      options: ['a', 'b'],
      answer_by: 'human',
      asked_at: 0,
    }
    const d = detail(
      [{ type: 'run_started' }],
      progress({
        waiting_on: 'gate',
        waiting_kind: 'human_review',
        pending_review: review,
        pending_reviews: [review],
        pending_questions: [question],
        pending_waits: ['w'],
      }),
      'running',
    )
    const gates = runGates(d)
    expect(gates.reviews).toEqual([{ node: 'gate', options: ['approve', 'reject'] }])
    expect(gates.questions).toEqual([{ node: 'ask', question: 'Which?', options: ['a', 'b'] }])
    expect(gates.waits).toEqual(['w'])
  })
})

describe('review recommendation', () => {
  const review = { node: 'g', options: ['approve', 'needs_changes'], instruction: 'i', how_to_decide: 'h' }
  const rec = { option: 'needs_changes', p: 0.861, confidence: 0.7, provider: 'main', model: 'jev-1.13.0', calibrated: true }

  it('is hidden when the gate has no recommendation', () => {
    const d = detail([], progress({ pending_reviews: [review] }), 'running')
    expect(runGates(d).reviews[0]).toEqual({ node: 'g', options: ['approve', 'needs_changes'] })
  })

  it('shows one advisory line when the gate carries one', () => {
    const d = detail([], progress({ pending_reviews: [{ ...review, recommendation: rec }] }), 'running')
    expect(runGates(d).reviews[0].recommendation).toBe(
      'Advisory recommendation: needs_changes (p=0.86), main/jev-1.13.0',
    )
  })

  it('marks an uncalibrated or engine-applied recommendation', () => {
    const d = detail(
      [],
      progress({ pending_reviews: [{ ...review, recommendation: { ...rec, calibrated: false, applied: true } }] }),
      'running',
    )
    expect(runGates(d).reviews[0].recommendation).toBe(
      'Decided: needs_changes (p=0.86), main/jev-1.13.0 [uncalibrated, applied by the engine]',
    )
  })

  it('shows no host task on a run that has none, and every one it has', () => {
    expect(runGates(detail([], progress(), 'running')).tasks).toEqual([])
    const pending = {
      run_id: 'r',
      task_id: 'plan-1',
      node: 'plan',
      attempt: 2,
      prompt: 'Plan it',
      role_prompt: 'You plan.',
      skills: [],
      workdir: '/w',
      outputs: null,
      deadline: null,
      model_hint: 'sonnet',
      env: {},
      requested_at: 1,
    }
    const d = detail([], progress({ waiting_kind: 'host_task', pending_tasks: [pending] }), 'running')
    expect(runGates(d).tasks).toEqual([
      {
        runId: 'r',
        taskId: 'plan-1',
        node: 'plan',
        attempt: 2,
        prompt: 'Plan it',
        rolePrompt: 'You plan.',
        modelHint: 'sonnet',
        deadline: null,
      },
    ])
  })

  it('reads a payload from a server without pending_tasks as none', () => {
    const p = progress()
    delete (p as Partial<ProgressSummary>).pending_tasks
    expect(runGates(detail([], p, 'running')).tasks).toEqual([])
  })
})

describe('deadlineNote', () => {
  it('names the time left, or overdue', () => {
    expect(deadlineNote(null, 0)).toBeNull()
    expect(deadlineNote(30_000, 0)).toBe('in 30s')
    expect(deadlineNote(600_000, 0)).toBe('in 10m')
    expect(deadlineNote(1_000, 5_000)).toBe('overdue')
  })
})
