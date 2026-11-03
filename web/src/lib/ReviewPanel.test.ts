// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ReviewPanel from './ReviewPanel.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('ReviewPanel', () => {
  // The next-actions panel leaves a gate's `decide_review` to this card, so
  // what an unattended run does there must show here, as on the CLI and MCP.
  it('shows what an unattended run does at each gate, marked where it stops', () => {
    app = mount(ReviewPanel, {
      target: document.body,
      props: {
        reviews: [
          { runId: 'r', node: 'auto', options: ['approve', 'reject'], unattended: { kind: 'decides', option: 'reject' } },
          { runId: 'r', node: 'plain', options: ['approve', 'reject'], unattended: { kind: 'fails_run', code: 'hitl_required' } },
          { runId: 'r', node: 'old', options: ['approve'] },
        ],
        onDecide: () => {},
      },
    })
    flushSync()
    const notes = [...document.querySelectorAll('[data-testid="review-unattended"]')].map((p) => [
      p.textContent?.trim(),
      p.classList.contains('stops'),
    ])
    expect(notes).toEqual([
      ['unattended: decides `reject` at once', false],
      ['unattended: fails the run (hitl_required)', true],
    ])
  })
})
