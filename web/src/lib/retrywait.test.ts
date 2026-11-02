import { describe, expect, it } from 'vitest'
import type { RetryWait } from './api.gen'
import { releasedLine, retryBadge, withRetryBadges } from './retrywait'

const fmt = (ms: number) => `T${ms}`

describe('retrywait', () => {
  it('says a released run waits to retry its nodes, not that its driver is dead', () => {
    expect(releasedLine({ seq: 9, wake_at_ms: 42, nodes: ['work', 'side'] }, fmt)).toBe(
      'Waiting to retry work, side at T42; a waker resumes the run then.',
    )
    expect(releasedLine({ seq: 9, wake_at_ms: 42, nodes: [] }, fmt)).toBe(
      'Waiting to retry at T42; a waker resumes the run then.',
    )
  })

  it('reads a retry due at a time: the short time on the card, the full one in its tooltip', () => {
    const short = (ms: number) => `S${ms}`
    const b = retryBadge({ node: 'w', attempt: 1, retry: 1, delay_ms: 60_000, due: { kind: 'at', at_ms: 7 } }, fmt, short)
    expect(b).toEqual({
      text: 'retry waiting · S7',
      title: 'retry 1 after attempt 1, delay 1m 0s; starts by itself at T7',
      frozen: false,
    })
  })

  it('reads a retry frozen while the run is paused, with what is left', () => {
    const b = retryBadge(
      { node: 'w', attempt: 1, retry: 1, delay_ms: 60_000, due: { kind: 'frozen', remaining_ms: 30_000 } },
      fmt,
    )
    expect(b).toEqual({
      text: 'retry frozen while paused · 30s',
      title: 'retry 1 after attempt 1, delay 1m 0s; frozen while the run is paused, 30s left',
      frozen: true,
    })
  })

  it('never reads a due kind it does not know as due now', () => {
    const due = { kind: 'later' } as unknown as RetryWait['due']
    const b = retryBadge({ node: 'w', attempt: 1, retry: 1, delay_ms: 5_000, due }, fmt)
    expect(b.text).toBe('retry waiting')
  })

  it('reads a retry due now', () => {
    const b = retryBadge({ node: 'w', attempt: 2, retry: 2, delay_ms: 5_000, due: { kind: 'now' } }, fmt)
    expect(b).toEqual({ text: 'retry due now', title: 'retry 2 after attempt 2, delay 5s; due now', frozen: false })
  })

  it('adds a waiting retry to the graph cards, with or without a card already there', () => {
    const cards = new Map<string, { runCount?: number }>([['work', { runCount: 2 }]])
    const out = withRetryBadges(cards, [
      { node: 'work', attempt: 1, retry: 1, delay_ms: 60_000, due: { kind: 'frozen', remaining_ms: 30_000 } },
      { node: 'side', attempt: 2, retry: 1, delay_ms: 5_000, due: { kind: 'now' } },
    ])
    expect(out.get('work')?.runCount).toBe(2)
    expect(out.get('work')?.retryWait?.frozen).toBe(true)
    expect(out.get('side')?.retryWait?.text).toBe('retry due now')
    expect(cards.get('work')).toEqual({ runCount: 2 })
    expect(cards.has('side')).toBe(false)
    expect(withRetryBadges(new Map(), undefined).size).toBe(0)
  })
})
