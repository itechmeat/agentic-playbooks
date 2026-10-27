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

  // A run whose inbox reads lost unread events says so as a warning, one line
  // per gap; a run without gaps shows no warning.
  it('warns about each inbox gap and stays quiet without one', () => {
    const gap = { node: 'watch', connector: 'echo-hooks', account: 'main', from: 3, through: 6 }
    app = mount(RunOutcomePanel, { target: document.body, props: { goal: null, commits: [], inboxGaps: [gap] } })
    flushSync()
    const lines = document.querySelectorAll('[data-testid="run-inbox-gap"]')
    expect(lines.length).toBe(1)
    expect(lines[0].textContent).toContain('events 3-6')
    expect(lines[0].textContent).toContain('echo-hooks/main')
    unmount(app)
    app = mount(RunOutcomePanel, { target: document.body, props: { goal: null, commits: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-inbox-gaps"]')).toBeNull()
  })
})
