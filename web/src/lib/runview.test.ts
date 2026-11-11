import { describe, it, expect } from 'vitest'
import type { ExecutionIdentityView, RunDetail } from './api.gen'
import type { WfEvent } from './types'
import {
  currentStage,
  decisionBars,
  lastNote,
  maxParallel,
  nodeRunData,
  parseExpectedDuration,
  runLogLines,
  runProfiles,
  runSteps,
  stepState,
  attemptModelText,
  attemptStepText,
} from './runview'

const ev = (seq: number, ts: number, type: string, extra: Record<string, unknown> = {}): WfEvent => ({
  seq,
  ts,
  type,
  ...extra,
})

// A CLI attempt row: the model apb requested.
const cli = (node: string, attempt: number, model: string, extra: Partial<ExecutionIdentityView> = {}): ExecutionIdentityView => ({
  node,
  attempt,
  executed_by: 'cli',
  agent: 'claude',
  requested_model: model,
  reported: { kind: 'none' },
  expected: 'opus',
  mismatch: false,
  ...extra,
})

// A host attempt row: what the host reported, the hint apart.
const host = (node: string, attempt: number, extra: Partial<ExecutionIdentityView> = {}): ExecutionIdentityView => ({
  node,
  attempt,
  executed_by: 'host',
  agent: 'host',
  reported: { kind: 'none' },
  expected: 'opus',
  mismatch: false,
  ...extra,
})

function detail(over: Partial<RunDetail> = {}): RunDetail {
  return {
    run_id: 'r1',
    playbook: 'pb',
    version: '1.0.0',
    run_status: 'running',
    failure_reason: null,
    driver_alive: true,
    nodes: { start: 'succeeded', build: 'running', check: 'pending', skip: 'skipped' },
    outputs: {},
    instruction: null,
    params: {},
    redacted_params: [],
    worktree: null,
    model: {
      id: 'pb',
      name: 'pb',
      nodes: [
        { id: 'start', type: 'start' },
        { id: 'build', type: 'agent_task', profile: 'developer', expected_duration: '10m' },
        { id: 'check', type: 'agent_task', profile: { name: 'reviewer', scope: 'project' } },
        { id: 'skip', type: 'script' },
      ],
      edges: [],
      defaults: { max_parallel: 2 } as never,
    },
    layout: null,
    graph: null,
    graph_model: null,
    hooks: {},
    children: [],
    outcome: { headline: 'Run running', done: [], verified: [], unverified: [], findings: [], warnings: [] },
    progress: null,
    answer: null,
    usage: null,
    unknown_events: 0,
    execution: 'cli',
    execution_fallback: false,
    concurrency: null,
    next_actions: { observed_at_ms: 0, status: 'running', stop: 'progresses_by_itself', actions: [] },
    events: [
      ev(0, 1_000, 'run_started'),
      ev(1, 1_000, 'node_started', { node: 'start', attempt: 1 }),
      ev(2, 1_000, 'node_finished', { node: 'start', status: 'succeeded' }),
      ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
      ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'claude', model: 'opus' }),
    ],
    attempt_models: [
      cli('build', 1, 'opus-5.5', { mismatch: true }),
    ],
    ...over,
  }
}

describe('runview', () => {
  it('parses the engine duration forms', () => {
    expect(parseExpectedDuration(90)).toBe(90)
    expect(parseExpectedDuration('5m')).toBe(300)
    expect(parseExpectedDuration('1h30m')).toBe(5400)
    expect(parseExpectedDuration('30m1h')).toBeUndefined()
    expect(parseExpectedDuration('soon')).toBeUndefined()
  })

  it('maps node statuses to rail markers', () => {
    expect(runSteps(detail()).map((s) => s.state)).toEqual(['done', 'current', 'pending', 'skipped'])
    expect(stepState('timed_out')).toBe('failed')
  })

  it('fills the graph card facts and caps the progress of a running node at 95', () => {
    const half = nodeRunData(detail(), 2_000 + 300_000).get('build')
    expect(half).toMatchObject({ profile: 'developer', model: 'opus-5.5', attempt: 1, progress: 50, duration: '5m 00s' })
    expect(nodeRunData(detail(), 2_000 + 3_600_000).get('build')?.progress).toBe(95)
    expect(nodeRunData(detail(), 5_000).get('check')).toEqual({ profile: 'reviewer' })
    // A retry has no node_started of its own; its share starts over.
    const retried = detail({
      events: [
        ...detail().events,
        ev(5, 200_000, 'attempt_finished', { node: 'build', attempt: 1, status: 'failed' }),
        ev(6, 302_000, 'attempt_started', { node: 'build', attempt: 2, agent: 'claude', model: 'opus' }),
      ],
    })
    expect(nodeRunData(retried, 302_000 + 60_000).get('build')?.progress).toBe(10)
    // A second visit of a looping node: its attempts restart at 1, and its
    // retry restarts the share although the first visit already reached 2.
    const revisited = detail({
      events: [
        ...retried.events,
        ev(7, 400_000, 'node_finished', { node: 'build', status: 'succeeded' }),
        ev(8, 500_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(9, 500_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'claude', model: 'opus' }),
        ev(10, 800_000, 'attempt_started', { node: 'build', attempt: 2, agent: 'claude', model: 'opus' }),
      ],
    })
    expect(nodeRunData(revisited, 800_000 + 60_000).get('build')?.progress).toBe(10)
  })

  it('builds one log line per event with the node profile as actor', () => {
    const lines = runLogLines(detail())
    expect(lines).toHaveLength(5)
    expect(lines[3]).toMatchObject({ actor: 'developer', message: 'node started · build · attempt 1' })
    expect(lines[0].actor).toBe('engine')
  })

  it('rates decisions sharp or split', () => {
    const events = [
      ev(1, 1, 'decision_made', { use_site: 'completion_check', node: 'a', mode: 'shadow', answers: { completion: { value: 'complete', p: 0.9, confidence: 0.94 } } }),
      ev(2, 2, 'decision_made', { use_site: 'judge', node: 'b', mode: 'enforce', answers: { x: { p: 0.53 } } }),
      ev(3, 3, 'decision_made', { use_site: 'judge', node: 'c', error: 'timeout' }),
    ]
    const bars = decisionBars(events)
    expect(bars.map((b) => [b.value, b.verdict])).toEqual([
      ['complete', 'sharp'],
      ['p=0.53', 'split'],
      ['error', 'error'],
    ])
    expect(bars[0].strength).toBeCloseTo(0.94)
  })

  it('reads profiles, stage, parallelism and the last note', () => {
    const d = detail()
    // The model the attempt actually ran on, the same one the graph card shows.
    expect(runProfiles(d)).toEqual([
      { name: 'developer', key: 'developer', model: 'opus-5.5' },
      { name: 'reviewer', scope: 'project', key: 'project:reviewer' },
    ])
    expect(currentStage(d)).toBe('build')
    expect(maxParallel(d)).toEqual({ value: 2, isDefault: false })
    expect(maxParallel(detail({ model: { ...d.model!, defaults: {} as never } }))).toEqual({ value: 4, isDefault: true })
    expect(lastNote([ev(1, 1, 'attempt_finished', { node: 'a', summary: 'did it' })])).toEqual({ node: 'a', text: 'did it' })
  })
})

describe('runview latest attempt', () => {
  it('shows the latest visit of a looping node, not its highest attempt', () => {
    // Visit 1 reached attempt 2 on a fallback model; visit 2 is on attempt 1
    // with the primary model.
    const d = detail({
      events: [
        ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'claude', model: 'opus' }),
        ev(5, 3_000, 'attempt_started', { node: 'build', attempt: 2, agent: 'claude', model: 'sonnet' }),
        ev(6, 4_000, 'node_finished', { node: 'build', status: 'succeeded' }),
        ev(7, 5_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(8, 5_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'claude', model: 'opus' }),
      ],
      attempt_models: [
        cli('build', 1, 'opus'),
        cli('build', 2, 'sonnet', { mismatch: true }),
        cli('build', 1, 'opus'),
      ],
    })
    expect(nodeRunData(d, 6_000).get('build')).toMatchObject({ model: 'opus', attempt: 1 })
  })

  it('keeps a host attempt without a reported model unreported, the request as a hint', () => {
    const d = detail({
      events: [
        ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host', model: 'opus-hint' }),
      ],
      attempt_models: [
        host('build', 1),
      ],
    })
    const b = nodeRunData(d, 3_000).get('build')
    expect(b?.model).toBeNull()
    expect(b?.modelHint).toBe('opus-hint')
    // No hint on the attempt: the profile's model is the hint.
    const bare = detail({
      events: [ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }), ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host' })],
      attempt_models: [host('build', 1)],
    })
    expect(nodeRunData(bare, 3_000).get('build')).toMatchObject({ model: null, modelHint: 'opus' })
  })

  it('takes the hint from the attempt only when it is the same attempt as the row', () => {
    // A host task request opened the row of attempt 2 before its
    // attempt_started: attempt 1's model is not its hint.
    const d = detail({
      events: [
        ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host', model: 'first-hint' }),
      ],
      attempt_models: [
        host('build', 1),
        host('build', 2),
      ],
    })
    expect(nodeRunData(d, 3_000).get('build')).toMatchObject({ model: null, modelHint: 'opus' })
  })

  it('treats a host attempt with no attempt_models row as not reported', () => {
    const d = detail({
      events: [
        ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
        ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host', model: 'host-hint' }),
      ],
      attempt_models: [],
    })
    expect(nodeRunData(d, 3_000).get('build')).toMatchObject({ model: null, modelHint: 'host-hint' })
    // A CLI attempt without a row still shows the model it launched with.
    const cliRun = detail({ attempt_models: [] })
    expect(nodeRunData(cliRun, 3_000).get('build')?.model).toBe('opus')
  })
})

describe('runview graph helpers', () => {
  it('reads an unknown node status as stopped part-way, not pending', () => {
    expect(stepState('unknown')).toBe('current')
  })
})

describe('runview attempt identity', () => {
  it('credits a host attempt with a model only when every task reported the same one', () => {
    const at = (reported: ExecutionIdentityView['reported']) =>
      nodeRunData(
        detail({
          events: [ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }), ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host', model: 'opus' })],
          attempt_models: [host('build', 1, { requested_model: 'opus', reported })],
        }),
        3_000,
      ).get('build')
    expect(at({ kind: 'one', model: 'glm-5.3-flash' })).toMatchObject({ model: 'glm-5.3-flash' })
    expect(at({ kind: 'partial', known: ['glm-5.3-flash'], unknown_tasks: 1 })).toMatchObject({ model: null, modelHint: 'opus' })
    expect(at({ kind: 'mixed', models: ['opus', 'sonnet'] })).toMatchObject({ model: null, modelHint: 'opus' })
  })

  it('takes a host attempt hint from its own row before the profile', () => {
    const d = detail({
      events: [ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 })],
      attempt_models: [host('build', 2, { requested_model: 'haiku' })],
    })
    expect(nodeRunData(d, 3_000).get('build')).toMatchObject({ model: null, modelHint: 'haiku' })
    expect(runProfiles(d)[0]).toEqual({ name: 'developer', key: 'developer', model: 'opus' })
  })

  it('words the model of each attempt without ever taking the hint for it', () => {
    expect(attemptModelText(cli('a', 1, 'sonnet'))).toBe('sonnet requested by apb')
    expect(attemptModelText(host('b', 1, { requested_model: 'opus', reported: { kind: 'one', model: 'glm-5.3-flash' } }))).toBe(
      'glm-5.3-flash reported by the host, hint opus',
    )
    expect(attemptModelText(host('b', 1, { requested_model: 'opus', reported: { kind: 'partial', known: ['glm-5.3-flash'], unknown_tasks: 2 } }))).toBe(
      'partial: glm-5.3-flash reported, 2 tasks not reported, hint opus',
    )
    expect(attemptModelText(host('c', 1, { reported: { kind: 'mixed', models: ['opus', 'sonnet'] } }))).toBe('mixed: opus, sonnet reported by the host')
    expect(attemptModelText(host('d', 1, { requested_model: 'opus' }))).toBe('not reported, hint opus')
  })

  it('names the chain position and why a step was entered, or that neither was recorded', () => {
    expect(attemptStepText(cli('a', 1, 'sonnet'))).toBe('position not recorded')
    expect(
      attemptStepText(
        cli('a', 2, 'haiku', {
          step: { binding: 'aaaaaaaaaaaa', step: 1, source: { kind: 'fallback', index: 1, of: 2 }, entered: { from_step: 0, closed_attempt: 1, reason: 'check_rejected' } },
        }),
      ),
    ).toBe('step 1 fallback 1 of 2, entered from step 0: check rejected')
    expect(attemptStepText(cli('a', 1, 'x', { step: { binding: 'b', step: 0, source: { kind: 'tier', name: 'fast' } } }))).toBe('step 0 tier fast')
    expect(attemptStepText(host('a', 1, { step: { binding: 'b', step: 2, source: { kind: 'host_fallback' }, entered: { from_step: 1, closed_attempt: 4, reason: 'not_recorded' } } }))).toBe(
      'step 2 host fallback, entered from step 1: not recorded',
    )
  })
})
