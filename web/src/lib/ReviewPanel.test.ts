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

// The scope of a review request (people-in-the-loop): what the decision
// covers, as the one line, and the options `max_scope` withholds.
describe('ReviewPanel scope', () => {
  beforeEach(() => auth.set({ required: true, authenticated: true, checked: true }))
  afterEach(() => auth.set({ required: false, authenticated: true, checked: false }))

  const scoped: PendingReview = {
    node: 'gate',
    instruction: 'Decide gate',
    options: ['approve', 'reject'],
    how_to_decide: '',
    scope_line: '3 commits, 6 files, +41 -38 lines',
    withheld: ['approve'],
    withheld_reason: 'the scope (90 files) is above max_scope (files 20); split the work or choose another option',
  }

  it('shows the scope line', () => {
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([scoped]), onDecide: () => {} } })
    flushSync()
    // The same label the CLI prints: the line says what the decision covers.
    expect(texts('[data-testid="review-scope"]')).toEqual(['scope: 3 commits, 6 files, +41 -38 lines'])
  })

  it('on a dashboard without keys offers no command for a withheld option, and names it withheld', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    const gate: PendingReview = { ...scoped, person_only: true }
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([gate]), onDecide: () => {} } })
    flushSync()
    expect(texts('[data-testid="review-cli-command"]')).toEqual(['apb review r1 gate --decision reject --request review:41'])
    expect(texts('[data-testid="review-withheld"]')).toEqual([
      'approve is withheld: the scope (90 files) is above max_scope (files 20); split the work or choose another option',
    ])
  })

  it('a withheld option is disabled with its reason', () => {
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([scoped]), onDecide: () => {} } })
    flushSync()
    const buttons = [...document.querySelectorAll<HTMLButtonElement>('[data-testid="review-option"]')]
    const approve = buttons.find((b) => b.textContent?.trim() === 'approve')
    const reject = buttons.find((b) => b.textContent?.trim() === 'reject')
    expect(approve?.disabled).toBe(true)
    expect(approve?.title).toContain('max_scope')
    expect(reject?.disabled).toBe(false)
    expect(texts('[data-testid="review-withheld"]')).toEqual([
      'approve is withheld: the scope (90 files) is above max_scope (files 20); split the work or choose another option',
    ])
  })
})

// Typed fields on a review gate (people-in-the-loop): the show entries and
// one input per field, the values sent with the decision.
describe('ReviewPanel fields', () => {
  beforeEach(() => auth.set({ required: true, authenticated: true, checked: true }))
  afterEach(() => auth.set({ required: false, authenticated: true, checked: false }))

  const fielded: PendingReview = {
    node: 'gate',
    instruction: 'Decide gate',
    options: ['approve', 'reject'],
    how_to_decide: '',
    show: [
      { label: 'Total', text: '1108.5' },
      { label: 'From the page', text: '<script>alert(1)</script>', untrusted: true },
    ],
    fields: [
      { name: 'amount', kind: 'number', required: ['approve'], default: 1108.5 },
      { name: 'currency', kind: 'enum', default: 'EUR', options: ['EUR', 'USD'] },
      { name: 'remark', kind: 'text', max_len: 200 },
      { name: 'sure', kind: 'bool', default: false },
    ],
  }

  it('renders one input per field', () => {
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([fielded]), onDecide: () => {} } })
    flushSync()
    expect(document.querySelectorAll('[data-testid="review-field-input"]')).toHaveLength(4)
    const amount = document.querySelector<HTMLInputElement>('[data-testid="review-field-amount"]')
    expect(amount?.value).toBe('1108.5')
    const select = document.querySelector<HTMLSelectElement>('[data-testid="review-field-currency"]')
    // The empty placeholder first, so an enum without a default chooses
    // nothing for the person.
    expect([...(select?.options ?? [])].map((o) => o.value)).toEqual(['', 'EUR', 'USD'])
    expect(select?.value).toBe('EUR')
  })

  it('names the options a field is required for', () => {
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([fielded]), onDecide: () => {} } })
    flushSync()
    expect(texts('[data-testid="review-field-input"] .fname')).toEqual([
      'amount (required for approve)',
      'currency',
      'remark',
      'sure',
    ])
  })

  it('marks an untrusted shown entry', () => {
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([fielded]), onDecide: () => {} } })
    flushSync()
    const shown = texts('[data-testid="review-shown"]')
    expect(shown).toEqual([
      'Total 1108.5',
      'From the page (untrusted text) <script>alert(1)</script>',
    ])
  })

  it('renders shown entries that share a label', () => {
    const twice: PendingReview = {
      ...fielded,
      show: [
        { label: 'Output', text: 'first' },
        { label: 'Output', text: 'second' },
      ],
    }
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([twice]), onDecide: () => {} } })
    flushSync()
    expect(texts('[data-testid="review-shown"]')).toEqual(['Output first', 'Output second'])
  })

  it('renders enum options that read the same after clipping', () => {
    const clipped: PendingReview = {
      ...fielded,
      fields: [{ name: 'pick', kind: 'enum', options: ['same prefix', 'same prefix'] }],
    }
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([clipped]), onDecide: () => {} } })
    flushSync()
    const select = document.querySelector<HTMLSelectElement>('[data-testid="review-field-pick"]')
    expect([...(select?.options ?? [])].map((o) => o.value)).toEqual(['', 'same prefix', 'same prefix'])
  })

  it('marks a field whose default renders untrusted text', () => {
    const tainted: PendingReview = {
      ...fielded,
      fields: [
        { name: 'note', kind: 'text', default: 'from the page', untrusted: true },
        { name: 'remark', kind: 'text' },
      ],
    }
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([tainted]), onDecide: () => {} } })
    flushSync()
    expect(texts('[data-testid="review-field-input"] .fname')).toEqual(['note (untrusted text)', 'remark'])
  })

  const field = <T extends HTMLElement>(name: string) =>
    document.querySelector<T>(`[data-testid="review-field-${name}"]`)!
  const type = (name: string, value: string) => {
    const el = field<HTMLInputElement>(name)
    el.value = value
    el.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
  }
  const click = (option: string) => {
    ;[...document.querySelectorAll<HTMLButtonElement>('[data-testid="review-option"]')]
      .find((b) => b.textContent?.trim() === option)!
      .click()
    flushSync()
  }

  it('sends the typed values with the decision, the other defaults kept', async () => {
    const onDecide = vi.fn()
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([fielded]), onDecide } })
    flushSync()
    type('amount', '1180.5')
    // The fields not edited still show their defaults after the edit.
    expect(field<HTMLSelectElement>('currency').value).toBe('EUR')
    expect(field<HTMLInputElement>('sure').checked).toBe(false)
    type('remark', 'checked')
    expect(field<HTMLInputElement>('amount').value).toBe('1180.5')
    expect(field<HTMLSelectElement>('currency').value).toBe('EUR')
    click('approve')
    await Promise.resolve()
    expect(onDecide).toHaveBeenCalled()
    const [review, option, payload] = onDecide.mock.calls[0]
    expect([review.node, option]).toEqual(['gate', 'approve'])
    expect(payload).toEqual({ amount: 1180.5, currency: 'EUR', remark: 'checked', sure: false })
  })

  it('a bool without a default shows "not chosen" and posts only what the person chose', () => {
    const unset: PendingReview = {
      ...fielded,
      fields: [
        { name: 'agreed', kind: 'bool', required: ['approve'] },
        { name: 'flag', kind: 'bool' },
      ],
    }
    const onDecide = vi.fn()
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([unset]), onDecide } })
    flushSync()
    const agreed = field<HTMLSelectElement>('agreed')
    // Not chosen is visible, marked, and can be chosen again.
    expect([...agreed.options].map((o) => [o.value, o.textContent?.trim(), o.disabled])).toEqual([
      ['', 'not chosen', false],
      ['true', 'true', false],
      ['false', 'false', false],
    ])
    expect(agreed.value).toBe('')
    expect(agreed.classList.contains('unset')).toBe(true)
    // Untouched: nothing posted for either, so the server's one check
    // refuses the required one, as on the CLI and MCP.
    click('approve')
    expect(onDecide.mock.calls[0][2]).toEqual({})
    const choose = (name: string, value: string) => {
      const el = field<HTMLSelectElement>(name)
      el.value = value
      el.dispatchEvent(new Event('change', { bubbles: true }))
      flushSync()
    }
    choose('agreed', 'false')
    expect(field<HTMLSelectElement>('agreed').classList.contains('unset')).toBe(false)
    choose('flag', 'true')
    click('approve')
    expect(onDecide.mock.calls[1][2]).toEqual({ agreed: false, flag: true })
    choose('flag', '')
    click('approve')
    expect(onDecide.mock.calls[2][2]).toEqual({ agreed: false })
  })

  it('a cleared required field is posted blank and the server decides', () => {
    // The panel never decides whether a required value is missing: it
    // posts the blank the person left, and the server's one check (the
    // same for the CLI, MCP and HTTP) refuses it for an approve.
    const onDecide = vi.fn()
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([fielded]), onDecide } })
    flushSync()
    type('amount', '')
    click('approve')
    expect(onDecide).toHaveBeenCalledTimes(1)
    expect(onDecide.mock.calls[0][1]).toBe('approve')
    expect(onDecide.mock.calls[0][2]).toMatchObject({ amount: '' })
    expect(texts('[data-testid="review-form-problem"]')).toEqual([])
  })

  it('a cleared default text is sent empty and text that is no number is named', () => {
    const withNote: PendingReview = {
      ...fielded,
      fields: [
        { name: 'amount', kind: 'number', default: 5 },
        { name: 'note', kind: 'text', default: 'from the run' },
      ],
    }
    const onDecide = vi.fn()
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([withNote]), onDecide } })
    flushSync()
    type('note', '')
    // Spaces only: no value, never 0.
    type('amount', '   ')
    click('reject')
    // Both cleared fields go blank to the server: the note stays empty, and
    // the amount, required for no decision, keeps its default there.
    expect(onDecide.mock.calls[0][2]).toEqual({ note: '', amount: '' })
    // A typed amount that reads as no whole number is never dropped
    // quietly, and never cut to the number it starts with.
    onDecide.mockClear()
    // A text input: a browser's number input drops the letters it cannot
    // take, so "12abc" would post as 12 with no word to the person.
    expect(field<HTMLInputElement>('amount').type).toBe('text')
    expect(field<HTMLInputElement>('amount').inputMode).toBe('decimal')
    type('amount', '12abc')
    expect(field<HTMLInputElement>('amount').value).toBe('12abc')
    click('reject')
    expect(onDecide).not.toHaveBeenCalled()
    expect(texts('[data-testid="review-form-problem"]')).toEqual(['amount: not a number'])
  })

  it('an enum without a default chooses nothing and posts nothing for it', () => {
    const plain: PendingReview = {
      ...fielded,
      fields: [{ name: 'currency', kind: 'enum', options: ['EUR', 'USD'] }],
    }
    const onDecide = vi.fn()
    app = mount(ReviewPanel, { target: document.body, props: { reviews: entries([plain]), onDecide } })
    flushSync()
    expect(field<HTMLSelectElement>('currency').value).toBe('')
    click('approve')
    expect(onDecide.mock.calls[0][2]).toEqual({})
  })
})
