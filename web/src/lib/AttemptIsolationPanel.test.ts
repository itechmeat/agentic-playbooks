// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import AttemptIsolationPanel from './AttemptIsolationPanel.svelte'
import { ISOLATION_CODE_TEXTS, ISOLATION_REASON_TEXTS } from './consts.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('AttemptIsolationPanel', () => {
  it('shows a badge per attempt with its limits and why a sandbox was missing', () => {
    const rows = [
      {
        node: 'build',
        attempt: 1,
        isolation: { level: 'full', backend: 'bwrap', network: 'none', holds: [], limits: ['user.host_uid', 'kernel.escapes'], view: [] },
      },
      {
        node: 'lint',
        attempt: 2,
        isolation: {
          level: 'best_effort',
          backend: 'none',
          network: 'open',
          holds: [],
          limits: ['no_sandbox'],
          fallbacks: ['disabled (APB_SANDBOX=off)'],
          view: [],
        },
      },
    ]
    app = mount(AttemptIsolationPanel, { target: document.body, props: { rows } })
    flushSync()
    const badges = [...document.querySelectorAll('[data-testid="run-isolation-badge"]')].map((b) => b.textContent?.trim())
    expect(badges).toEqual(['full · sandbox · network none', 'best_effort · no sandbox · network open'])
    const limits = [...document.querySelectorAll('[data-testid="run-isolation-limit"]')].map((l) => l.textContent ?? '')
    expect(limits.length).toBe(3)
    expect(limits[0]).toContain(ISOLATION_CODE_TEXTS['user.host_uid'])
    const text = document.body.textContent ?? ''
    expect(text).toContain(ISOLATION_REASON_TEXTS.disabled)
    expect(text).toContain('#2')
  })

  it('renders nothing for a run without an isolated attempt', () => {
    app = mount(AttemptIsolationPanel, { target: document.body, props: { rows: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-isolation"]')).toBeNull()
  })
})
