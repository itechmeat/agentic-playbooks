// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import AttemptModelsPanel from './AttemptModelsPanel.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('AttemptModelsPanel', () => {
  it('lists each attempt with its model and flags a mismatch with the profile', () => {
    const models = [
      { node: 'plan', attempt: 1, executed_by: 'host', agent: null, model: 'GLM-5.3-Flash', expected: 'opus', mismatch: true },
      { node: 'build', attempt: 1, executed_by: 'cli', agent: 'claude', model: 'sonnet', expected: 'sonnet', mismatch: false },
      { node: 'build', attempt: 2, executed_by: 'host', agent: null, model: null, expected: 'sonnet', mismatch: false },
    ]
    app = mount(AttemptModelsPanel, { target: document.body, props: { models } })
    flushSync()
    const rows = [...document.querySelectorAll('[data-testid="run-model-row"]')].map((r) => r.textContent ?? '')
    expect(rows.length).toBe(3)
    expect(rows[0]).toContain('GLM-5.3-Flash')
    expect(rows[0]).toContain('profile: opus')
    expect(rows[2]).toContain('not reported')
    expect(document.querySelector('[data-testid="run-models-mismatch"]')?.textContent).toContain('1 differ')
  })

  it('renders a re-run node whose attempt number repeats', () => {
    const row = { node: 'review', attempt: 1, executed_by: 'host', agent: null, model: 'opus', expected: 'opus', mismatch: false }
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
})
