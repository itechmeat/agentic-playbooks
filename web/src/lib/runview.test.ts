import { describe, it, expect } from 'vitest'
import type { RunDetail } from './api.gen'
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
} from './runview'

const ev = (seq: number, ts: number, type: string, extra: Record<string, unknown> = {}): WfEvent => ({
  seq,
  ts,
  type,
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
    hooks: {},
    children: [],
    outcome: { headline: 'Run running', done: [], verified: [], unverified: [], findings: [], warnings: [] },
    progress: null,
    answer: null,
    usage: null,
    unknown_events: 0,
    execution: 'cli',
    execution_fallback: false,
    events: [
      ev(0, 1_000, 'run_started'),
      ev(1, 1_000, 'node_started', { node: 'start', attempt: 1 }),
      ev(2, 1_000, 'node_finished', { node: 'start', status: 'succeeded' }),
      ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }),
      ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'claude', model: 'opus' }),
    ],
    attempt_models: [
      { node: 'build', attempt: 1, executed_by: 'cli', agent: 'claude', model: 'opus-5.5', expected: 'opus', mismatch: true },
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
        { node: 'build', attempt: 1, executed_by: 'cli', agent: 'claude', model: 'opus', expected: 'opus', mismatch: false },
        { node: 'build', attempt: 2, executed_by: 'cli', agent: 'claude', model: 'sonnet', expected: 'opus', mismatch: true },
        { node: 'build', attempt: 1, executed_by: 'cli', agent: 'claude', model: 'opus', expected: 'opus', mismatch: false },
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
        { node: 'build', attempt: 1, executed_by: 'host', agent: null, model: null, expected: 'opus', mismatch: false },
      ],
    })
    const b = nodeRunData(d, 3_000).get('build')
    expect(b?.model).toBeNull()
    expect(b?.modelHint).toBe('opus-hint')
    // No hint on the attempt: the profile's model is the hint.
    const bare = detail({
      events: [ev(3, 2_000, 'node_started', { node: 'build', attempt: 1 }), ev(4, 2_000, 'attempt_started', { node: 'build', attempt: 1, agent: 'host' })],
      attempt_models: [{ node: 'build', attempt: 1, executed_by: 'host', agent: null, model: null, expected: 'opus', mismatch: false }],
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
        { node: 'build', attempt: 1, executed_by: 'host', agent: null, model: null, expected: 'opus', mismatch: false },
        { node: 'build', attempt: 2, executed_by: 'host', agent: null, model: null, expected: 'opus', mismatch: false },
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
    const cli = detail({ attempt_models: [] })
    expect(nodeRunData(cli, 3_000).get('build')?.model).toBe('opus')
  })
})

describe('runview graph helpers', () => {
  it('marks walked, active and idle edges', async () => {
    const { edgeRunState } = await import('./runview')
    const state = edgeRunState([ev(1, 1, 'edge_traversed', { from: 'a', to: 'b' })], {
      a: 'succeeded',
      b: 'running',
      c: 'pending',
    })
    expect(state('a', 'b')).toBe('active')
    expect(state('b', 'c')).toBe('idle')
    expect(edgeRunState([ev(1, 1, 'edge_traversed', { from: 'a', to: 'c' })], { a: 'succeeded', c: 'succeeded' })('a', 'c')).toBe('walked')
  })

  it('lights only the edge that led back into a looping node', async () => {
    const { edgeRunState } = await import('./runview')
    const state = edgeRunState(
      [
        ev(1, 1, 'edge_traversed', { from: 'start', to: 'build' }),
        ev(2, 2, 'node_finished', { node: 'build', status: 'succeeded' }),
        ev(3, 3, 'edge_traversed', { from: 'build', to: 'review' }),
        ev(4, 4, 'node_finished', { node: 'review', status: 'failed' }),
        ev(5, 5, 'edge_traversed', { from: 'review', to: 'build' }),
      ],
      { start: 'succeeded', build: 'running', review: 'failed' },
    )
    expect(state('review', 'build')).toBe('active')
    expect(state('start', 'build')).toBe('walked')
  })

  it('reads an unknown node status as stopped part-way, not pending', () => {
    expect(stepState('unknown')).toBe('current')
  })
})
