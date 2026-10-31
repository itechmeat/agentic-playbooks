// Shared display formatters: one duration format and one model name format
// for every page, so a run card, a list cell and a legend never disagree.

/**
 * A duration at a glance: `13 ms` below a second, then `42s`, `6m 05s`,
 * `1h 52m`, `3d 4h`. Every unit above milliseconds is floored (an elapsed
 * time never reads longer than it was); milliseconds round. A negative
 * value reads as zero and a non-finite one as `-`.
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms)) return '-'
  const v = Math.round(Math.max(0, ms))
  if (v < 1000) return `${v} ms`
  const s = Math.floor(v / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, '0')}s`
  const h = Math.floor(m / 60)
  if (h < 48) return `${h}h ${String(m % 60).padStart(2, '0')}m`
  return `${Math.floor(h / 24)}d ${h % 24}h`
}

/**
 * A model id as the dashboard shows it: `claude-opus-5-5` reads `opus 5.5`,
 * `claude-sonnet-5` reads `sonnet 5`, a dated `claude-sonnet-4-20250514`
 * reads `sonnet 4`; anything else (other providers, provider paths,
 * mixed-case names) stays as written. Show the raw id in a `title` next to it.
 */
export function shortModel(model: string): string {
  const m = /^claude-([a-z]+)-(\d+)(?:[-.](\d{1,2}))?(?:-\d{8})?$/.exec(model.trim())
  if (!m) return model
  return `${m[1]} ${m[2]}${m[3] ? `.${m[3]}` : ''}`
}
