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

  it('renders nothing before the first attempt', () => {
    app = mount(AttemptModelsPanel, { target: document.body, props: { models: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-models"]')).toBeNull()
  })
})
