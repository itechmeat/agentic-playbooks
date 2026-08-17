import { describe, it, expect } from 'vitest'
import { pendingReviews } from './reviews'
import type { WfEvent } from './types'

const ev = (seq: number, type: string, extra: Record<string, unknown> = {}): WfEvent =>
  ({ seq, ts: 0, type, ...extra }) as WfEvent

describe('pendingReviews', () => {
  it('surfaces a requested but not yet decided review', () => {
    const events = [ev(0, 'review_requested', { node: 'gate', options: ['approved', 'rejected'] })]
    expect(pendingReviews(events)).toEqual([{ node: 'gate', options: ['approved', 'rejected'] }])
  })

  it('hides a review once decided', () => {
    const events = [
      ev(0, 'review_requested', { node: 'gate', options: ['approved'] }),
      ev(1, 'review_decided', { node: 'gate', decision: 'approved' }),
    ]
    expect(pendingReviews(events)).toEqual([])
  })

  it('re-surfaces a second visit that is requested again', () => {
    const events = [
      ev(0, 'review_requested', { node: 'gate', options: ['a'] }),
      ev(1, 'review_decided', { node: 'gate', decision: 'a' }),
      ev(2, 'review_requested', { node: 'gate', options: ['a'] }),
    ]
    expect(pendingReviews(events)).toEqual([{ node: 'gate', options: ['a'] }])
  })

  // issue #102.9: the gate's optional prompt is carried through to the
  // review panel, shown above the options.
  it('carries the gate prompt when present', () => {
    const events = [
      ev(0, 'review_requested', {
        node: 'gate',
        options: ['approved', 'rejected'],
        prompt: 'Check the changelog before deciding.',
      }),
    ]
    expect(pendingReviews(events)).toEqual([
      { node: 'gate', options: ['approved', 'rejected'], prompt: 'Check the changelog before deciding.' },
    ])
  })

  it('omits prompt when the gate has none', () => {
    const events = [ev(0, 'review_requested', { node: 'gate', options: ['a'] })]
    expect(pendingReviews(events)).toEqual([{ node: 'gate', options: ['a'] }])
  })
})
