// Usage stats for one connector (design doc section 9's usage-stats bullet),
// aggregated server-side from recent run event logs.
export interface ConnectorFunctionStat {
  function: string
  account: string
  calls: number
  errors: number
  avgDurationMs: number
}

export interface ConnectorStats {
  connector: string
  runsScanned: number
  calls: number
  byFunction: ConnectorFunctionStat[]
  byOutcome: Record<string, number>
}

// Error rate as a whole percentage. 0/0 reads as "0%" (a stat with no calls
// is not an error rate of NaN or 100%).
export function errorRate(stat: ConnectorFunctionStat): string {
  if (stat.calls === 0) return '0%'
  return `${Math.round((stat.errors / stat.calls) * 100)}%`
}

// A stable, human-readable summary line for the outcome tally, e.g.
// "ok: 5, auth: 1, network: 2". "ok" always sorts first (the expected,
// common case); the rest sort alphabetically so the line does not reorder
// itself between renders. Zero-count entries are dropped, and an entirely
// empty/zero tally reads as "no recorded calls" rather than an empty string.
export function outcomeSummary(byOutcome: Record<string, number>): string {
  const entries = Object.entries(byOutcome).filter(([, n]) => n > 0)
  if (entries.length === 0) return 'no recorded calls'
  entries.sort(([a], [b]) => {
    if (a === 'ok') return -1
    if (b === 'ok') return 1
    return a.localeCompare(b)
  })
  return entries.map(([code, n]) => `${code}: ${n}`).join(', ')
}

// Renders a duration in whole milliseconds; a non-finite input (should not
// happen, but a stat with zero calls could otherwise divide to NaN upstream)
// renders as a dash instead of "NaN ms".
export function formatDurationMs(ms: number): string {
  if (!Number.isFinite(ms)) return '-'
  return `${Math.round(ms)} ms`
}
