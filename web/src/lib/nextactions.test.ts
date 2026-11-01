import { describe, expect, it } from 'vitest'
import { render } from 'svelte/server'
import type { FitReport, NextAction, StartAction } from './api.gen'
import {
  autoLine,
  cliLine,
  durationWords,
  pageControls,
  refusalStartActions,
  remedyLine,
  startRefusalOf,
  shownElsewhere,
} from './nextactions'
import FitCard from './FitCard.svelte'
import { ApiError } from './api/http'

const action = (over: Partial<NextAction>): NextAction => ({
  code: 'read_report',
  run_id: 'r1',
  target: {},
  args: [],
  principals: ['person'],
  ready: true,
  reason: '',
  ...over,
})

describe('cliLine', () => {
  it('shows the line the server rendered, and none for a supervisor action', () => {
    const line = "apb answer --node=ask --request=question:12 r1 '<your answer>'"
    expect(cliLine(action({ code: 'answer_question', command: line }))).toBe(line)
    expect(cliLine(action({ code: 'supervisor_abort' }))).toBeNull()
  })
})

describe('autoLine', () => {
  it('says when a wait continues by itself, and when that is not assured', () => {
    const fmt = (ms: number) => `T${ms}`
    expect(autoLine({ kind: 'at', at_ms: 5, outcome: 'timer_elapses', assured: true }, fmt)).toBe(
      'continues by itself at T5: the timer elapses',
    )
    expect(autoLine({ kind: 'at', at_ms: 5, outcome: 'resume_attempt', assured: false }, fmt)).toBe(
      'continues by itself at T5 (not assured): apb tries to resume the run',
    )
  })

  it('names the time left of a frozen timer', () => {
    expect(durationWords(5_000)).toBe('5s')
    expect(durationWords(3_725_000)).toBe('1h 2m')
    expect(autoLine({ kind: 'frozen', remaining_ms: 61_000, outcome: 'wait_times_out' })).toBe(
      'timer frozen while paused, 1m 1s left: then the wait times out and fails',
    )
  })
})

const start = (over: Partial<StartAction>): StartAction => ({
  code: 'fix_requirement',
  playbook: 'p',
  args: [],
  principals: ['person'],
  reason: 'r',
  ...over,
})

describe('start actions', () => {
  it('says how each remedy is met', () => {
    expect(remedyLine(start({ remedy: { remedy: 'approve_digest', kind: 'connector_bundle', digest: 'sha256:a' } }))).toBe(
      'approve the connector bundle at digest sha256:a',
    )
    expect(remedyLine(start({ remedy: { remedy: 'provide_param', name: 'target' } }))).toBe(
      'provide a value for the param target',
    )
    expect(remedyLine(start({ remedy: { remedy: 'install_agent', agent: 'codex' } }))).toBe('install the codex CLI')
    expect(remedyLine(start({ remedy: { remedy: 'set_env', name: 'TOKEN' } }))).toBe(
      'set the environment variable TOKEN',
    )
    expect(remedyLine(start({ remedy: { remedy: 'run_doctor' } }))).toBe('run apb doctor to see what this machine lacks')
    expect(remedyLine(start({ remedy: { remedy: 'manual', detail: 'log in first' } }))).toBe('log in first')
    expect(remedyLine(start({}))).toBeNull()
  })

  it('reads the start actions of a refusal body', () => {
    expect(refusalStartActions(undefined)).toEqual([])
    expect(refusalStartActions({ error: 'x' })).toEqual([])
    expect(refusalStartActions({ start_actions: [start({})] })).toHaveLength(1)
  })

  it('shows them in the fit report', () => {
    const report: FitReport = {
      id: 'p',
      scope: 'project',
      version: '1.0.0',
      ready: false,
      trusted: true,
      needs_consent: false,
      compatibility: [],
      setup: [],
      trust: [],
      consent: { irreversible: [], effects: [] },
      start_actions: [start({ reason: 'the param target has no value', remedy: { remedy: 'provide_param', name: 'target' } })],
    }
    const { body } = render(FitCard, { props: { report } })
    expect(body).toContain('data-testid="fit-start-actions"')
    expect(body).toContain('the param target has no value')
    expect(body).toContain('provide a value for the param target')
  })
})

describe('controls shown elsewhere', () => {
  const controls = pageControls(
    { questions: [{ node: 'ask' }], reviews: [{ node: 'gate' }], publishes: [{ node: 'ship' }] },
    true,
  )
  const at = (code: NextAction['code'], node?: string, run = 'r1') =>
    action({ code, run_id: run, target: node ? { node } : {} })

  it("names the own run's question, review and publish gates by node, and the resume", () => {
    expect(shownElsewhere(at('answer_question', 'ask'), 'r1', controls)).toBe(true)
    expect(shownElsewhere(at('decide_review', 'gate'), 'r1', controls)).toBe(true)
    expect(shownElsewhere(at('decide_publish', 'ship'), 'r1', controls)).toBe(true)
    expect(shownElsewhere(at('resume_run'), 'r1', controls)).toBe(true)
    expect(shownElsewhere(at('resume_with_budget'), 'r1', controls)).toBe(true)
  })

  it('keeps another node, a child run, an unknown send and a resume the page does not offer', () => {
    expect(shownElsewhere(at('decide_review', 'other'), 'r1', controls)).toBe(false)
    expect(shownElsewhere(at('decide_review', 'gate', 'child'), 'r1', controls)).toBe(false)
    expect(shownElsewhere(at('resolve_intent', 'send'), 'r1', controls)).toBe(false)
    const none = pageControls({ questions: [], reviews: [], publishes: [] }, false)
    expect(shownElsewhere(at('resume_run'), 'r1', none)).toBe(false)
  })
})

describe('startRefusalOf', () => {
  const action: StartAction = {
    code: 'fix_requirement',
    playbook: 'p',
    args: [],
    principals: ['person'],
    reason: 'the param target needs a value',
    remedy: { remedy: 'provide_param', name: 'target' },
  }

  it("keeps the server's explanation with the refusal's actions", () => {
    const e = new ApiError('refused', 409, 'param_needs_value', undefined, {
      detail: 'no default and nothing remembered for target',
      start_actions: [action],
    })
    expect(startRefusalOf(e)).toEqual({
      message: 'The run was refused (param_needs_value): no default and nothing remembered for target',
      actions: [action],
    })
  })

  it('leaves no panel for a refusal without start actions or another error', () => {
    expect(startRefusalOf(new ApiError('refused', 409, 'budget_needs_decision', 'x', {}))).toBeNull()
    expect(startRefusalOf(new Error('network'))).toBeNull()
  })
})
