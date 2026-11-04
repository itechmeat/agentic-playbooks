// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ReviewPanel from './ReviewPanel.svelte'
import RunResultsPanel from './RunResultsPanel.svelte'
import { auth } from './auth.svelte'
import { runGates, type ReviewEntry } from './rungates'
import type { ResultsView } from './results'
import type { PendingReview } from './api.gen'
import type { RunDetail } from './types'

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

// Bound and person-only gates (PR 20b). The fixtures are what the server's
// `PendingReview` carries, run through `runGates` like the page does, so a
// field the mapping drops fails here too.
const snapA = 'a'.repeat(64)
const snapB = 'b'.repeat(64)
const bound: PendingReview = {
  node: 'review',
  instruction: 'Decide review',
  options: ['approve', 'reject'],
  how_to_decide: '',
  person_only: true,
  binding: {
    nonce: 'approval-3fcd30582598bd5fa956cc2f0ea538ed',
    snapshots: [
      { node: 'implement', execution: 2, settlement_seq: 40, snapshot_id: snapA, incomplete: true, omitted: 3 },
      { node: 'items', execution: 1, item: 'pr-7', settlement_seq: 12, snapshot_id: snapB, redacted: true },
    ],
    missing: [{ node: 'docs', reason: 'not_run' }],
  },
}

function entries(reviews: PendingReview[], seq = 41): ReviewEntry[] {
  const detail = {
    run_id: 'r1',
    progress: { pending_reviews: reviews, pending_questions: [], pending_waits: [], pending_supervisor: null },
    next_actions: {
      actions: reviews.map((r) => ({
        code: 'decide_review',
        run_id: 'r1',
        target: { node: r.node, request: { kind: 'review', seq, nonce: r.binding?.nonce } },
        args: [],
      })),
    },
  } as unknown as RunDetail
  return runGates(detail).reviews
}

function texts(sel: string): string[] {
  return [...document.querySelectorAll(sel)].map((e) => (e.textContent ?? '').replace(/\s+/g, ' ').trim())
}
const buttons = () => texts('[data-testid="review-option"]')

describe('ReviewPanel bound and person-only gates', () => {
  beforeEach(() => auth.set({ required: true, authenticated: true, checked: true }))
  afterEach(() => auth.set({ required: false, authenticated: true, checked: false }))

  it('lists the bound snapshots and the missing nodes, each snapshot linked to its row in the results panel', () => {
    const view: ResultsView = {
      rows: [snapA, snapB].map((id, i) => ({
        execution: { node: i ? 'items' : 'implement', ordinal: 1 },
        settlement_seq: 10 + i,
        status: 'succeeded',
        snapshot_id: id,
        entries: [],
        omitted: [],
        provenance: { cache_hit: false },
        verified: 'ok',
      })),
      legacy: [],
      not_stored: [],
    }
    const results = document.createElement('div')
    const panel = document.createElement('div')
    document.body.append(panel, results)
    const other = mount(RunResultsPanel, { target: results, props: { runId: 'r1', view } })
    app = mount(ReviewPanel, { target: panel, props: { reviews: entries([bound]), onDecide: () => {} } })
    flushSync()
    try {
      expect(texts('[data-testid="review-binding-snapshot"]')).toEqual([
        `implement, execution 2 ${snapA.slice(0, 12)} incomplete 3 omitted Show in results`,
        `items, execution 1, item pr-7 ${snapB.slice(0, 12)} redacted Show in results`,
      ])
      expect(texts('[data-testid="review-binding-missing"]')).toEqual([
        'docs: not run; approving does not approve it',
      ])
      expect(texts('[data-testid="review-binding-nonce"]')).toEqual(['binding approval-3fcd30582598bd5fa956cc2f0ea538ed'])
      // The link lands on the row of that very snapshot in the results panel.
      const spy = vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {})
      document.querySelectorAll<HTMLButtonElement>('[data-testid="review-binding-link"]')[1].click()
      const rows = document.querySelectorAll('[data-testid="run-results-row"]')
      expect(spy.mock.contexts).toEqual([rows[1]])
      spy.mockRestore()
    } finally {
      unmount(other)
    }
  })

  it('on a dashboard without keys shows the terminal command per option for a person-only gate, and no buttons', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    const plain: PendingReview = { node: 'plain', instruction: '', options: ['approve', 'reject'], how_to_decide: '' }
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([bound, plain]), onDecide: () => {} } })
    flushSync()
    const [gate, other] = [...document.querySelectorAll('.gate')]
    expect(gate.querySelectorAll('[data-testid="review-option"]')).toHaveLength(0)
    expect([...gate.querySelectorAll('[data-testid="review-cli-command"]')].map((c) => c.textContent)).toEqual([
      'apb review r1 review --decision approve --nonce approval-3fcd30582598bd5fa956cc2f0ea538ed --request review:41',
      'apb review r1 review --decision reject --nonce approval-3fcd30582598bd5fa956cc2f0ea538ed --request review:41',
    ])
    expect(gate.querySelector('[data-testid="review-cli"]')?.textContent).toContain('apb server key issue')
    // An ordinary gate on the same dashboard keeps its buttons.
    expect([...other.querySelectorAll('[data-testid="review-option"]')].map((b) => b.textContent?.trim())).toEqual([
      'approve',
      'reject',
    ])
  })

  it('starts the terminal command in the run\'s project directory, since apb review reads the project from there', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    app = mount(ReviewPanel, {
      target: document.body,
      props: { reviews: entries([bound]), onDecide: () => {}, projectDir: '/home/me/my project' },
    })
    flushSync()
    expect(document.querySelector('[data-testid="review-cli-command"]')?.textContent).toBe(
      "cd '/home/me/my project' && apb review r1 review --decision approve --nonce approval-3fcd30582598bd5fa956cc2f0ea538ed --request review:41",
    )
  })

  it('shows no command while the project directory is still being looked up, since one without it may run elsewhere', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    app = mount(ReviewPanel, {
      target: document.body,
      props: { reviews: entries([bound]), onDecide: () => {}, projectDir: null },
    })
    flushSync()
    expect(document.querySelectorAll('[data-testid="review-cli-command"]')).toHaveLength(0)
    expect(document.querySelector('[data-testid="review-cli-pending"]')?.textContent).toContain(
      "Looking up the run's project directory",
    )
  })

  it('says to run the command from the project when the run\'s project directory could not be found', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    app = mount(ReviewPanel, {
      target: document.body,
      props: { reviews: entries([bound]), onDecide: () => {}, projectDir: '', projectDirMissing: true },
    })
    flushSync()
    expect(document.querySelector('[data-testid="review-cli-no-dir"]')?.textContent).toContain(
      'run the command from that project',
    )
    expect(document.querySelectorAll('[data-testid="review-cli-command"]')).toHaveLength(2)
  })

  it('with a signed-in session decides a person-only gate by its buttons with the binding nonce, and shows the server refusal', () => {
    const onDecide = vi.fn()
    const reviews = entries([bound])
    app = mount(ReviewPanel, {
      target: document.body,
      props: {
        reviews,
        onDecide,
        refusals: {
          'r1:review:41': 'gate `review` of run `r1` is person_only, so only a person decides it.',
          // Another run's gate of the same name, and an earlier request.
          'r2:review:41': 'not this gate',
          'r1:review:40': 'not this request',
        },
      },
    })
    flushSync()
    expect(document.querySelector('[data-testid="review-cli"]')).toBeNull()
    expect(texts('[data-testid="review-person-only"]')).toEqual([
      'Only a person decides this gate: at a terminal, or here while signed in.',
    ])
    expect(buttons()).toEqual(['approve', 'reject'])
    document.querySelector<HTMLButtonElement>('[data-testid="review-option"]')?.click()
    expect(onDecide).toHaveBeenCalledWith(reviews[0], 'approve')
    expect(onDecide.mock.calls[0][0]).toMatchObject({
      runId: 'r1',
      node: 'review',
      request: 41,
      nonce: 'approval-3fcd30582598bd5fa956cc2f0ea538ed',
    })
    expect(texts('[data-testid="review-refusal"]')).toEqual([
      'gate `review` of run `r1` is person_only, so only a person decides it.',
    ])
  })
})
