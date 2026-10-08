import { describe, expect, it } from 'vitest'
import { render } from 'svelte/server'
import PlaybookStatsCard from './PlaybookStatsCard.svelte'
import { modelsText, perRunText, rateText, spendText, versionsNewestFirst, waitsText } from './playbookstats'
import type { StatsReport, VersionStats } from './api.gen'

const version = (v: string, extra: Partial<VersionStats> = {}): VersionStats => ({
  playbook: 'p',
  version: v,
  runs: 3,
  outcomes: { succeeded: 2, failed: 1, aborted: 0, other: 0 },
  success: { count: 2, of: 3, rate: 0.6667 },
  first_pass: { count: 1, of: 3, rate: 0.3333 },
  retries: { total: 1, runs: 3, per_run: 0.33 },
  fallbacks: { total: 0, runs: 3, per_run: 0 },
  loop_traversals: { total: 1, runs: 3, per_run: 0.33 },
  gate_wait: { count: 1, median_ms: 60_000, max_ms: 60_000 },
  question_wait: { count: 0 },
  duration: { count: 3, median_ms: 5_000, max_ms: 82_000 },
  spend: { runs_with_usage: 0, tokens: { total: 0, runs: 0 }, runs_with_cost: 0, cost_usd: 0 },
  deliverable_missing: 0,
  output_fields_missing: 0,
  goal: [],
  models: {},
  model_mismatch: 0,
  nodes: [],
  note: '3 runs: fewer than 10, the rates are indicative only',
  ...extra,
})

const report: StatsReport = {
  runs: 4,
  versions: [
    version('1.0.0'),
    version('1.1.0', {
      runs: 4,
      goal: [
        { index: 0, description: 'tests pass', check: 'script', checked: 1, passed: { count: 1, of: 1, rate: 1 }, failed: 0, errors: 0, manual: 0 },
        { index: 1, description: 'a person reads it', check: 'manual', checked: 1, passed: { count: 0, of: 0 }, failed: 0, errors: 0, manual: 1 },
      ],
      spend: { runs_with_usage: 1, tokens: { total: 1500, runs: 1, per_run: 1500 }, runs_with_cost: 1, cost_usd: 0.02, cost_per_run_usd: 0.02 },
    }),
  ],
}

describe('playbook stats formatting', () => {
  it('shows counts next to rates', () => {
    expect(rateText({ count: 2, of: 3, rate: 0.6667 })).toBe('2/3 (67%)')
    expect(rateText({ count: 0, of: 0 })).toBe('0/0')
    expect(perRunText({ total: 1, runs: 3, per_run: 0.33 })).toBe('0.33 per run (1 over 3 runs)')
    expect(perRunText({ total: 0, runs: 0 })).toBe('0')
    expect(waitsText({ count: 1, median_ms: 60_000 })).toBe('median 1.0 min over 1')
    expect(waitsText({ count: 0 })).toBe('none')
    expect(spendText(report.versions[0])).toBeNull()
    expect(spendText(report.versions[1])).toBe(
      '1500.00 tokens per run (1500 over 1 runs), $0.0200 per run reported by 1 of 4 runs',
    )
    expect(versionsNewestFirst(report).map((v) => v.version)).toEqual(['1.1.0', '1.0.0'])
    expect(versionsNewestFirst(null)).toEqual([])
  })
})

describe('PlaybookStatsCard', () => {
  const html = (props: Record<string, unknown>) => render(PlaybookStatsCard, { props }).body

  it('renders every version newest first, with goal results only where checked', () => {
    const body = html({ report })
    expect(body.match(/data-testid="playbook-stats-version"/g)?.length).toBe(2)
    expect(body.indexOf('1.1.0')).toBeLessThan(body.indexOf('1.0.0'))
    expect(body).toContain('2/3 (67%)')
    expect(body).toContain('passed 1/1 (100%)')
    expect(body).toContain('manual in 1')
    expect(body.match(/data-testid="playbook-stats-goal"/g)?.length).toBe(1)
    expect(body).toContain('$0.0200 per run reported by 1 of 4 runs')
    expect(body).toContain('0.33 per run (1 over 3 runs)')
  })

  it('has a loading, an error and an empty state', () => {
    expect(html({ loading: true })).toContain('playbook-stats-loading')
    expect(html({ error: 'HTTP 500' })).toContain('Could not load the stats: HTTP 500')
    expect(html({ report: { runs: 0, versions: [], note: 'no runs recorded' } })).toContain('playbook-stats-empty')
    expect(html({})).toContain('playbook-stats-empty')
    // A reload keeps the previous report on screen instead of a spinner.
    expect(html({ report, loading: true })).not.toContain('playbook-stats-loading')
  })
})

describe('modelsText', () => {
  it('lists the models attempts ran on and how many differ from the profile', () => {
    expect(modelsText({ models: {}, model_mismatch: 0 })).toBeNull()
    expect(modelsText({ models: { opus: 1, 'glm-5.3-flash': 5 }, model_mismatch: 5 })).toBe(
      'glm-5.3-flash x5, opus x1 (5 differ from the profile)',
    )
  })
})
