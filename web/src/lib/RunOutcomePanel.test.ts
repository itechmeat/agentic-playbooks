// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import RunOutcomePanel from './RunOutcomePanel.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('RunOutcomePanel', () => {
  // A node can list the same commit in two records (it ran twice): each
  // record is a row, and the list never fails on a duplicate key.
  it('lists a commit a node reported twice once per report', () => {
    const sha = 'a'.repeat(40)
    const record = { node: 'fix', before: 'b'.repeat(40), after: sha, commits: [{ sha, subject: 'fix it' }], omitted: 0 }
    app = mount(RunOutcomePanel, { target: document.body, props: { goal: null, commits: [record, record] } })
    flushSync()
    expect(document.querySelectorAll('[data-testid="run-commit-row"]').length).toBe(2)
  })

  it('says which attempt made a commit', () => {
    const sha = 'a'.repeat(40)
    const record = {
      node: 'fix',
      before: 'b'.repeat(40),
      after: sha,
      commits: [{ sha, subject: 'fix it' }],
      omitted: 0,
      by_attempt: [{ id: 9, number: 3 }],
    }
    app = mount(RunOutcomePanel, { target: document.body, props: { goal: null, commits: [record] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-commit-attempt"]')?.textContent).toBe('committed by attempt 3')
    // A spaced separator before it, as before the node.
    expect(document.querySelector('[data-testid="run-commit-row"]')?.textContent).toContain(' · committed by attempt 3')
  })
})
