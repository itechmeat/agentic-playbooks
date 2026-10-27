// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import RunKnownProblemsCard from './RunKnownProblemsCard.svelte'
import type { RunKnownProblems } from './api.gen'
import { hasRunProblems } from './knownproblems'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

function render(problems: RunKnownProblems) {
  app = mount(RunKnownProblemsCard, {
    target: document.body,
    props: { problems, workspace: '/w', playbook: 'fix-build' },
  })
  flushSync()
}

const q = (id: string) => [...document.querySelectorAll(`[data-testid="${id}"]`)]

describe('RunKnownProblemsCard', () => {
  it('lists what the run showed and hid, why, and the unsigned count', () => {
    render({
      shown: [
        {
          id: 'kp-1',
          scope: 'connector:crm.update_contact',
          symptom: 'update_contact returned rate_limited',
          workaround: 'wait a minute',
          nodes: ['file_tasks'],
          basis: 'model',
        },
      ],
      hidden: [
        { id: 'kp-2', reason: 'source_unreadable' },
        { id: 'kp-3', reason: 'no_matching_node' },
      ],
      unsigned: 2,
    })
    const shown = q('run-known-problem-shown')
    expect(shown.length).toBe(1)
    expect(shown[0].textContent).toContain('Workaround reported: wait a minute')
    expect(shown[0].textContent).toContain('to file_tasks, rechecked by the decision model')
    const hidden = q('run-known-problem-hidden').map((h) => h.textContent)
    expect(hidden[0]).toContain('its source cannot be read now')
    expect(hidden[1]).toContain('no agent node of this run')
    expect(q('run-known-problems-unsigned')[0].textContent).toContain('2 problems in a file')
    const link = document.querySelector('a') as HTMLAnchorElement
    expect(link.getAttribute('href')).toBe('#/playbook/%2Fw/fix-build')
  })

  it('shows no unsigned line without one, and the run page has no block when nothing happened', () => {
    render({ shown: [], hidden: [{ id: 'kp-2', reason: 'over_limit' }], unsigned: 0 })
    expect(q('run-known-problems-unsigned').length).toBe(0)
    expect(hasRunProblems({ shown: [], hidden: [], unsigned: 0 })).toBe(false)
  })
})
