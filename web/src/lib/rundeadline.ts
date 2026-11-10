// The one line a run surface shows for the run's deadline. Matches the
// CLI's `deadline:` line in `apb runs <id>`: the total, what is left and
// when it ends, or that the clock is frozen, or that it was exceeded.
import type { RunDeadlineView } from './api.gen'

// A duration in milliseconds as an author writes one: `45m`, `1h30m`.
export function durationText(ms: number): string {
  let secs = Math.floor(ms / 1000)
  if (secs === 0) return '0s'
  let out = ''
  for (const [unit, size] of [
    ['d', 86400],
    ['h', 3600],
    ['m', 60],
    ['s', 1],
  ] as const) {
    const n = Math.floor(secs / size)
    if (n > 0) {
      out += `${n}${unit}`
      secs -= n * size
    }
  }
  return out
}

export function deadlineLine(d: RunDeadlineView): string {
  const total = durationText(d.total_ms)
  if (d.remaining_ms === 0) return `Deadline ${total}, exceeded`
  const left = durationText(d.remaining_ms)
  return d.ends_at
    ? `Deadline ${total}, ${left} left (ends ${d.ends_at})`
    : `Deadline ${total}, ${left} left, frozen (paused by a person)`
}
