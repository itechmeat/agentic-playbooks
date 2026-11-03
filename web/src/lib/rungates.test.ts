import { describe, expect, it } from 'vitest'
import { deadlineNote, runGates } from './rungates'
import type { ProgressSummary, RunDetail, WfEvent } from './types'
import type { NextAction, PendingHostTask, PendingQuestion, RunActions } from './api.gen'

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

const detail = (
  events: Partial<WfEvent>[],
  p: ProgressSummary,
  status: RunDetail['run_status'],
  actions: NextAction[] = [],
): RunDetail => ({
  run_id: 'r',
  playbook: 'p',
  version: '1.0.0',
  run_status: status,
  failure_reason: null,
  outcome: { headline: 'Run running', done: [], verified: [], unverified: [], findings: [], warnings: [] },
  driver_alive: null,
  nodes: {},
  outputs: {},
  instruction: null,
  params: {},
  redacted_params: [],
  worktree: null,
  model: null,
  layout: null,
  graph: null,
  graph_model: null,
  hooks: {},
  children: [],
  progress: p,
  answer: null,
  usage: null,
  unknown_events: 0,
  execution: 'cli',
  execution_fallback: false,
  next_actions: { observed_at_ms: 0, status, stop: 'needs_input', actions } satisfies RunActions,
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
    expect(gates.reviews).toEqual([{ node: 'gate', options: ['approve', 'reject'], runId: 'r' }])
    expect(gates.questions).toEqual([{ node: 'ask', question: 'Which?', options: ['a', 'b'], runId: 'r' }])
    expect(gates.waits).toEqual(['w'])
  })
})

describe('review recommendation', () => {
  const review = { node: 'g', options: ['approve', 'needs_changes'], instruction: 'i', how_to_decide: 'h' }
  const rec = { option: 'needs_changes', p: 0.861, confidence: 0.7, provider: 'main', model: 'jev-1.13.0', calibrated: true }

  it('is hidden when the gate has no recommendation', () => {
    const d = detail([], progress({ pending_reviews: [review] }), 'running')
    expect(runGates(d).reviews[0]).toEqual({ node: 'g', options: ['approve', 'needs_changes'], runId: 'r' })
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
      prompt_path: '/r/tasks/plan-1/prompt.md',
      role_path: '/r/tasks/plan-1/role.md',
      skills: [],
      workdir: '/w',
      outputs: null,
      deadline: null,
      model_hint: 'sonnet',
      hint_source: { kind: 'fallback' as const, index: 1, of: 1, profile: 'main' },
      fallback_of: { attempt: 1, reason: 'failed' },
      hint_note: 'NOTE',
      execution_note: 'CONTRACT',
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
        hintNote: 'NOTE',
        executionNote: 'CONTRACT',
        deadline: null,
      },
    ])
  })

  it('reads a task without a hint note from an older server as none', () => {
    const pending = {
      run_id: 'r', task_id: 'plan-1', node: 'plan', attempt: 1, prompt: 'p', role_prompt: null,
      skills: [], workdir: '/w', outputs: null, deadline: null, model_hint: null, env: {}, requested_at: 1,
    } as unknown as PendingHostTask
    const d = detail([], progress({ waiting_kind: 'host_task', pending_tasks: [pending] }), 'running')
    expect(runGates(d).tasks[0].hintNote).toBeNull()
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

describe('publish gates', () => {
  const plan = {
    url: 'file:///srv/r.git',
    branch: 'apb/out',
    after: 'b'.repeat(40),
    commits: [{ sha: 'b'.repeat(40), subject: 'work', this_run: true }],
    omitted: 0,
    this_run: 1,
    outside_push_suspected: false,
    plan_digest: 'sha256:x',
    nonce: 'publish-0123',
  }
  const review = { node: 'gate', options: ['approve', 'reject'], instruction: 'i', how_to_decide: 'h' }
  const publish = { node: 'ship', options: ['publish', 'skip'], instruction: 'p', how_to_decide: 'h', publish: plan, prompt: 'Check the subjects.' }

  it('routes a review carrying a plan to the publish panel, never the review card', () => {
    const d = detail([], progress({ pending_reviews: [review, publish] }), 'running')
    const gates = runGates(d)
    expect(gates.reviews.map((r) => r.node)).toEqual(['gate'])
    expect(gates.publishes).toEqual([{ node: 'ship', plan, prompt: 'Check the subjects.', runId: 'r' }])
  })

  it('is empty without progress', () => {
    expect(runGates(detail([], null as unknown as ProgressSummary, 'running')).publishes).toEqual([])
  })

  // Each gate carries the request it answers: the run id and the seq of the
  // run's own matching next action, and a publish gate the nonce of its plan.
  // A child's action of the same node is never taken for the page's own.
  it('carries the run id, request seq and unattended outcome of the matching next action', () => {
    const act = (code: NextAction['code'], node: string, run_id: string, seq: number, kind: 'question' | 'review' | 'publish'): NextAction => ({
      code,
      run_id,
      target: { node, request: { kind, seq } },
      args: [],
      principals: ['person'],
      ready: true,
      reason: '',
    })
    const question: PendingQuestion = { node: 'ask', question: 'Which?', options: [], answer_by: 'human', asked_at: 0 }
    const d = detail(
      [],
      progress({ pending_reviews: [review, publish], pending_questions: [question] }),
      'running',
      [
        { ...act('decide_review', 'gate', 'child-1', 99, 'review'), unattended: { kind: 'decides', option: 'reject' } },
        { ...act('decide_review', 'gate', 'r', 7, 'review'), unattended: { kind: 'fails_run', code: 'hitl_required' } },
        { ...act('decide_publish', 'ship', 'r', 9, 'publish'), unattended: { kind: 'fails_run', code: 'hitl_required' } },
        { ...act('answer_question', 'ask', 'r', 12, 'question'), unattended: { kind: 'answers_default' } },
      ],
    )
    const gates = runGates(d)
    // Each card draws its gate's action, so it also carries what an
    // unattended run does there.
    expect(gates.reviews[0]).toMatchObject({
      node: 'gate',
      runId: 'r',
      request: 7,
      unattended: { kind: 'fails_run', code: 'hitl_required' },
    })
    expect(gates.publishes[0]).toMatchObject({
      node: 'ship',
      runId: 'r',
      request: 9,
      plan: { nonce: 'publish-0123' },
      unattended: { kind: 'fails_run', code: 'hitl_required' },
    })
    expect(gates.questions[0]).toMatchObject({ node: 'ask', runId: 'r', request: 12, unattended: { kind: 'answers_default' } })
  })

  it('names no request when the run lists no matching action', () => {
    const d = detail([], progress({ pending_reviews: [review] }), 'running', [])
    expect(runGates(d).reviews[0]).toEqual({ node: 'gate', options: ['approve', 'reject'], runId: 'r' })
  })
})
