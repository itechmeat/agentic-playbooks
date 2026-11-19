// @vitest-environment happy-dom
// A looping gate asks again with a new request: the panel starts that
// request from its own rendered defaults, never from what was typed for the
// request before it. Runes need the `.svelte.` infix.
import { describe, it, expect, afterEach, beforeEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ReviewPanel from './ReviewPanel.svelte'
import { auth } from './auth.svelte'
import type { ReviewEntry } from './rungates'

let app: ReturnType<typeof mount> | null = null
beforeEach(() => auth.set({ required: true, authenticated: true, checked: true }))
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  auth.set({ required: false, authenticated: true, checked: false })
})

const visit = (request: number, amount: number): ReviewEntry => ({
  runId: 'r1',
  node: 'gate',
  request,
  options: ['approve', 'reject'],
  fields: [{ name: 'amount', kind: 'number', default: amount }],
})

describe('ReviewPanel on a looping gate', () => {
  it('a new request shows its own defaults, not the values typed for the last one', () => {
    const props = $state({ reviews: [visit(7, 100)], onDecide: () => {} })
    app = mount(ReviewPanel, { target: document.body, props })
    flushSync()
    const amount = () => document.querySelector<HTMLInputElement>('[data-testid="review-field-amount"]')!
    amount().value = '150'
    amount().dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    expect(amount().value).toBe('150')
    // The gate was decided and the run looped back: a new request.
    props.reviews = [visit(12, 200)]
    flushSync()
    expect(amount().value).toBe('200')
  })
})
