// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ExecutorReport from './ExecutorReport.svelte'
import type { ExecutorGroup, StepStats } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const step = (over: Partial<StepStats> = {}): StepStats => ({
  binding: 'aaaaaaaaaaaa',
  step: 0,
  source: 'primary',
  executed_by: 'cli',
  attempts: 2,
  successes: { count: 0, of: 2, rate: 0 },
  cost_known: { count: 0, of: 2 },
  reported_models: {},
  closes_by_reason: { check_rejected: 2 },
  ...over,
})

const cliGroup: ExecutorGroup = {
  profile: 'p1',
  bundle: 'b1',
  executor: 'x1',
  ephemeral: false,
  runs: 2,
  attempts: 4,
  steps: [step(), step({ step: 1, source: 'fallback', successes: { count: 2, of: 2, rate: 1 }, closes_by_reason: {} }), step({ step: 2, source: 'fallback', attempts: 0 })],
  transitions: [
    { from_step: 0, to_step: 1, reason: 'check_rejected', count: 1 },
    { from_step: 0, to_step: 1, reason: 'not_recorded', count: 1 },
  ],
}

const hostGroup: ExecutorGroup = {
  profile: 'p2',
  bundle: 'b2',
  executor: 'x2',
  ephemeral: true,
  runs: 1,
  attempts: 1,
  steps: [step({ executed_by: 'host', attempts: 1, successes: { count: 1, of: 1, rate: 1 }, closes_by_reason: {}, reported_models: { partial: 1 } })],
  transitions: [],
  hint_follow: { hinted: 1, followed: 0, not_followed: 0, not_reported: 1 },
}

describe('ExecutorReport', () => {
  it('shows each group with its step rows, transitions and the hint line of a host group', () => {
    app = mount(ExecutorReport, { target: document.body, props: { node: 'build', groups: [cliGroup, hostGroup] } })
    flushSync()
    const groups = [...document.querySelectorAll('[data-testid="executor-group"]')]
    expect(groups.length).toBe(2)
    const head = groups[0].querySelector('[data-testid="executor-group-head"]')?.textContent ?? ''
    expect(head).toContain('executor x1 (profile p1, bundle b1): 2 runs, 4 attempts')
    const steps = [...groups[0].querySelectorAll('[data-testid="executor-step"]')].map((r) => r.textContent?.replace(/\s+/g, ' ').trim())
    expect(steps).toEqual([
      'step 0 primary (cli) 2 attempts, succeeded 0/2 (0%), closes: check rejected x2',
      'step 1 fallback (cli) 2 attempts, succeeded 2/2 (100%)',
      'step 2 fallback (cli) never ran',
    ])
    const moves = [...groups[0].querySelectorAll('[data-testid="executor-transition"]')].map((r) => r.textContent?.trim())
    expect(moves).toEqual(['step 0 to step 1: check rejected x1', 'step 0 to step 1: not recorded x1'])
    // A transition without a recorded reason is marked as such, never reclassified.
    expect([...groups[0].querySelectorAll('[data-testid="executor-transition"].unrecorded')].map((r) => r.textContent?.trim())).toEqual([
      'step 0 to step 1: not recorded x1',
    ])
    expect(groups[0].querySelector('[data-testid="executor-hints"]')).toBeNull()
    expect(groups[1].querySelector('[data-testid="executor-group-head"]')?.textContent).toContain('executor x2, override')
    expect(groups[1].querySelector('[data-testid="executor-hints"]')?.textContent).toBe(
      'hints: 1 hinted, 0 followed, 0 not followed, 1 not reported',
    )
    expect(groups[1].textContent).toContain('models: partial x1')
  })

  it('renders nothing without a group', () => {
    app = mount(ExecutorReport, { target: document.body, props: { node: 'build', groups: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="executor-report"]')).toBeNull()
  })
})
