import type { PerRun, Rate, StatsReport, VersionStats, Waits } from './api.gen'

// The playbook page's stats card (C3): pure formatting of `GET /api/stats`.
// Every rate is shown with its count, because small samples mislead.

// `7/9 (78%)`, or `0/0` when nothing was counted.
export function rateText(r: Rate): string {
  return r.rate === undefined ? `${r.count}/${r.of}` : `${r.count}/${r.of} (${Math.round(r.rate * 100)}%)`
}

// `0.33 per run (1 over 3 runs)`, or `0` when there were no runs. The same
// text as `apb stats`.
export function perRunText(p: PerRun): string {
  return p.per_run === undefined ? '0' : `${p.per_run.toFixed(2)} per run (${p.total} over ${p.runs} runs)`
}

function dur(ms: number): string {
  const s = ms / 1000
  if (s < 60) return `${Math.round(s)} s`
  if (s < 3600) return `${(s / 60).toFixed(1)} min`
  return `${(s / 3600).toFixed(1)} h`
}

// `median 1.0 min over 3`, or `none`.
export function waitsText(w: Waits): string {
  return w.median_ms === undefined ? 'none' : `median ${dur(w.median_ms)} over ${w.count}`
}

// `1234 tokens per run (1234 over 1 runs), $0.0200 per run reported by 1 of
// 4 runs`, or null without reported usage. The averages cover only the runs
// that reported usage or cost, so both say how many, as `apb stats` does.
export function spendText(v: VersionStats): string | null {
  const s = v.spend
  if (s.runs_with_usage === 0) return null
  const t = s.tokens
  let line = `${(t.per_run ?? 0).toFixed(2)} tokens per run (${t.total} over ${t.runs} runs)`
  if (s.cost_per_run_usd !== undefined) {
    line += `, $${s.cost_per_run_usd.toFixed(4)} per run reported by ${s.runs_with_cost} of ${v.runs} runs`
  }
  return line
}

// The versions newest first (the API lists them oldest first).
export function versionsNewestFirst(r: StatsReport | null): VersionStats[] {
  return r ? [...r.versions].reverse() : []
}
