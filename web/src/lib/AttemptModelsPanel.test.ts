// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import AttemptModelsPanel from './AttemptModelsPanel.svelte'
import type { ExecutionIdentityView } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('AttemptModelsPanel', () => {
  it('lists each attempt with the model apb requested or the host reported and flags a mismatch', () => {
    const models: ExecutionIdentityView[] = [
      { node: 'plan', attempt: 1, executed_by: 'host', agent: 'host', requested_model: 'opus', reported: { kind: 'one', model: 'GLM-5.3-Flash' }, expected: 'opus', mismatch: true },
      { node: 'build', attempt: 1, executed_by: 'cli', agent: 'claude', requested_model: 'sonnet', reported: { kind: 'none' }, expected: 'sonnet', mismatch: false, step: { binding: 'aaaaaaaaaaaa', step: 0, source: { kind: 'primary' } } },
      { node: 'build', attempt: 2, executed_by: 'cli', agent: 'claude', requested_model: 'haiku', reported: { kind: 'none' }, expected: 'sonnet', mismatch: true, step: { binding: 'aaaaaaaaaaaa', step: 1, source: { kind: 'fallback', index: 1, of: 1 }, entered: { from_step: 0, closed_attempt: 1, reason: 'check_rejected' } } },
      { node: 'check', attempt: 1, executed_by: 'host', agent: 'host', requested_model: 'opus', reported: { kind: 'partial', known: ['glm-5.3-flash'], unknown_tasks: 1 }, expected: 'opus', mismatch: false },
      { node: 'fix', attempt: 1, executed_by: 'host', agent: 'host', requested_model: 'opus', reported: { kind: 'mixed', models: ['opus', 'sonnet'] }, expected: 'opus', mismatch: false },
      { node: 'ship', attempt: 1, executed_by: 'host', agent: 'host', requested_model: 'opus', reported: { kind: 'none' }, expected: 'opus', mismatch: false },
    ]
    app = mount(AttemptModelsPanel, { target: document.body, props: { models } })
    flushSync()
    const rows = [...document.querySelectorAll('[data-testid="run-model-row"]')].map((r) => r.textContent?.replace(/\s+/g, ' ') ?? '')
    expect(rows.length).toBe(6)
    expect(rows[0]).toContain('GLM-5.3-Flash reported by the host, hint opus')
    expect(rows[0]).toContain('profile: opus')
    expect(rows[1]).toContain('sonnet requested by apb')
    expect(rows[1]).toContain('step 0 primary')
    expect(rows[2]).toContain('step 1 fallback 1 of 1, entered from step 0: check rejected')
    expect(rows[3]).toContain('partial: glm-5.3-flash reported, 1 task not reported, hint opus')
    expect(rows[4]).toContain('mixed: opus, sonnet reported by the host')
    expect(rows[5]).toContain('not reported, hint opus')
    expect(rows[5]).not.toContain('opus reported')
    expect(document.querySelector('[data-testid="run-models-mismatch"]')?.textContent).toContain('2 differ')
  })

  it("shows the run's hint follow line under the rows, and none without hints", () => {
    const models: ExecutionIdentityView[] = [
      { node: 'plan', attempt: 1, executed_by: 'host', agent: 'host', requested_model: 'opus', reported: { kind: 'one', model: 'sonnet' }, expected: 'opus', mismatch: true },
    ]
    app = mount(AttemptModelsPanel, { target: document.body, props: { models, hintFollow: { hinted: 1, followed: 0, not_followed: 1, not_reported: 0 } } })
    flushSync()
    expect(document.querySelector('[data-testid="run-models-hints"]')?.textContent).toBe(
      'hints: 1 hinted, 0 followed, 1 not followed, 0 not reported',
    )
    unmount(app)
    app = mount(AttemptModelsPanel, { target: document.body, props: { models } })
    flushSync()
    expect(document.querySelector('[data-testid="run-models-hints"]')).toBeNull()
  })

  it('renders a re-run node whose attempt number repeats', () => {
    const row: ExecutionIdentityView = { node: 'review', attempt: 1, executed_by: 'host', agent: 'host', reported: { kind: 'one', model: 'opus' }, expected: 'opus', mismatch: false }
    app = mount(AttemptModelsPanel, { target: document.body, props: { models: [row, { ...row }] } })
    flushSync()
    expect(document.querySelectorAll('[data-testid="run-model-row"]').length).toBe(2)
  })

  it('renders nothing before the first attempt', () => {
    app = mount(AttemptModelsPanel, { target: document.body, props: { models: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-models"]')).toBeNull()
  })

  it('shows the current attempt alone, before any model row', () => {
    const current = {
      node: 'build',
      profile: 'developer',
      agent: 'claude',
      model: 'claude-opus-5-5',
      attempt: 2,
      progress: 40,
      status: 'running',
      tail: ['wrote the plan', 'transcript: t.jsonl'],
    }
    app = mount(AttemptModelsPanel, { target: document.body, props: { models: [], current } })
    flushSync()
    const head = document.querySelector('[data-testid="run-current-attempt"]')?.parentElement?.textContent ?? ''
    for (const part of ['build', 'developer', 'claude', 'opus 5.5', 'attempt 2', '40%', 'running']) expect(head).toContain(part)
    expect(document.querySelector('pre')?.textContent).toContain('wrote the plan')
    expect(document.querySelectorAll('[data-testid="run-model-row"]').length).toBe(0)
  })

  it('leaves out the attempt, progress and status it does not know', () => {
    app = mount(AttemptModelsPanel, { target: document.body, props: { current: { node: 'plan', tail: [] } } })
    flushSync()
    const head = document.querySelector('[data-testid="run-current-attempt"]')
    expect(head?.textContent).toContain('plan')
    const text = head?.nextElementSibling?.textContent ?? ''
    expect(text).not.toContain('attempt')
    expect(text).not.toContain('%')
    expect(document.querySelector('pre')).toBeNull()
  })

  it('says a model was not reported and marks the request as a hint', () => {
    app = mount(AttemptModelsPanel, {
      target: document.body,
      props: { current: { node: 'build', agent: 'host', modelHint: 'claude-opus-5-5', tail: [] } },
    })
    flushSync()
    const text = document.querySelector('[data-testid="run-current-attempt"]')?.nextElementSibling?.textContent ?? ''
    expect(text).toContain('model not reported (hint opus 5.5)')
  })

  it('says a running attempt is still awaiting its model report', () => {
    app = mount(AttemptModelsPanel, {
      target: document.body,
      props: { current: { node: 'build', agent: 'host', modelHint: 'claude-opus-5-5', status: 'running', tail: [] } },
    })
    flushSync()
    const text = document.querySelector('[data-testid="run-current-attempt"]')?.nextElementSibling?.textContent ?? ''
    expect(text).toContain('model awaiting report (hint opus 5.5)')
  })
})
