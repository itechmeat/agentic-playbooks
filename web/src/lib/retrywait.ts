// Deferred retries on the run page: the badge of a node whose retry waits
// out its `retry_delay`, and the line of a run whose driver let go while
// such a retry waits (a waker resumes it). Read only from the run detail's
// `retry_waits` and `released`.
import type { Released, RetryWait } from './api.gen'
import { durationWords, localTime } from './nextactions'

/** The clock time of `ms`, with its date when it is not today. */
function clockTime(ms: number): string {
  const at = new Date(ms)
  const time = { hour: '2-digit', minute: '2-digit' } as const
  return at.toDateString() === new Date().toDateString()
    ? at.toLocaleTimeString([], time)
    : at.toLocaleString([], { month: 'short', day: 'numeric', ...time })
}

/** The card badge of a waiting retry: when it is due (the clock time on the
 * card, with the date when not today, the full time in its tooltip), or
 * frozen while the run is paused with what is left. A due kind a newer
 * server added reads as a plain wait, never as due. */
export function retryBadge(
  w: RetryWait,
  fmt: (ms: number) => string = localTime,
  short: (ms: number) => string = clockTime,
): { text: string; title: string; frozen: boolean } {
  const retry = `retry ${w.retry} after attempt ${w.attempt}, delay ${durationWords(w.delay_ms)}`
  switch (w.due.kind) {
    case 'frozen':
      return {
        text: `retry frozen while paused · ${durationWords(w.due.remaining_ms)}`,
        title: `${retry}; frozen while the run is paused, ${durationWords(w.due.remaining_ms)} left`,
        frozen: true,
      }
    case 'at':
      return {
        text: `retry waiting · ${short(w.due.at_ms)}`,
        title: `${retry}; starts by itself at ${fmt(w.due.at_ms)}`,
        frozen: false,
      }
    case 'now':
      return { text: 'retry due now', title: `${retry}; due now`, frozen: false }
    default:
      return { text: 'retry waiting', title: retry, frozen: false }
  }
}

/** The line of a released run, instead of "driver dead". */
export function releasedLine(r: Released, fmt: (ms: number) => string = localTime): string {
  const what = r.nodes.length ? `Waiting to retry ${r.nodes.join(', ')}` : 'Waiting to retry'
  return `${what} at ${fmt(r.wake_at_ms)}; a waker resumes the run then.`
}

/** The graph cards of the nodes whose retry waits: a copy of `cards` with
 * each waiting node's retry badge added (a node with no card yet gets one
 * holding only the badge, so `T` should be a partial card). What the run page
 * hands its graph, with or without a graph; `cards` is left as it was. */
export function withRetryBadges<T extends object>(
  cards: Map<string, T>,
  waits: readonly RetryWait[] | undefined,
): Map<string, T & { retryWait?: ReturnType<typeof retryBadge> }> {
  type Card = T & { retryWait?: ReturnType<typeof retryBadge> }
  const out = new Map(cards) as Map<string, Card>
  for (const w of waits ?? []) out.set(w.node, { ...out.get(w.node), retryWait: retryBadge(w) } as Card)
  return out
}
