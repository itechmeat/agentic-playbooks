import { describe, expect, it } from 'vitest'
import { render } from 'svelte/server'
import PlaybookStatsCard from './PlaybookStatsCard.svelte'
import {
  categoryText,
  categoryLabel,
  interventionsHeadText,
  interventionsPartlyKnown,
  kindsText,
  knownCountText,
  withoutAnyText,
  costKnownText,
  closeReasonWord,
  digestText,
  executorGroups,
  executorReports,
  groupHeadText,
  hintFollowText,
  stepLabelText,
  stepText,
  transitionText,
  modelsText,
  nodeLayer,
  nodeStatsOf,
  nodeStatsText,
  perRunText,
  reasonClassText,
  reasonLines,
  previousVersion,
  qualityDeltaText,
  qualityDeltas,
  qualityLabel,
  qualityLabels,
  qualityExecutorTexts,
  qualityText,
  rateText,
  spendText,
  versionsNewestFirst,
  waitsText,
} from './playbookstats'
import type { ExecutorGroup, KnownCount, NodeStats, QualityStats, StatsReport, StepStats, VersionStats } from './api.gen'

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
  spend: { runs_with_usage: 0, tokens: { total: 0, runs: 0 }, runs_with_cost: 0, cost_known: { count: 0, of: 0 }, cost_usd: 0 },
  deliverable_missing: 0,
  output_fields_missing: 0,
  goal: [],
  findings: { by_kind: [], top: [], runs_with_warnings: { count: 0, of: 3 } },
  baseline: [],
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
      spend: { runs_with_usage: 1, tokens: { total: 1500, runs: 1, per_run: 1500 }, runs_with_cost: 1, cost_known: { count: 1, of: 1, rate: 1 }, cost_usd: 0.02, cost_per_run_usd: 0.02, cost_by_basis: { subscription_equivalent_usd: 0.02 } },
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
      '1500.00 tokens per run (1500 over 1 runs), $0.0200 per run reported by 1 of 4 runs (in all $0.0200 subscription equivalent, not billed)',
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
    expect(body).toContain('cost known')
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

describe('costKnownText', () => {
  it('gives the share of attempts that reported a cost, and nothing before any attempt', () => {
    const spend = report.versions[1].spend
    expect(costKnownText({ spend: { ...spend, cost_known: { count: 3, of: 4, rate: 0.75 } } })).toBe('3/4 (75%) attempts')
    expect(costKnownText({ spend: { ...spend, cost_known: { count: 0, of: 0 } } })).toBeNull()
  })
})

describe('executor digests', () => {
  const group = (profile: string, runs: number, tokens?: number) => ({
    profile,
    bundle: `${profile}b`,
    runs,
    first_pass: { count: runs, of: runs, rate: 1 },
    duration: { count: runs, median_ms: 60_000, max_ms: 60_000 },
    tokens_per_visit: tokens,
  })
  const node = (name: string, digests: ReturnType<typeof group>[]) => ({
    node: name,
    runs: 2,
    in_runs: 2,
    in_runs_unknown: 0,
    not_run: [],
    executions: 2,
    completed: 2,
    open: 0,
    failed: 0,
    cancelled: 0,
    first_pass: { count: 2, of: 2, rate: 1 },
    retries: 0,
    fallbacks: 0,
    reentries: 0,
    duration: { count: 2, median_ms: 60_000, max_ms: 60_000 },
    deliverable_missing: 0,
    output_fields_missing: 0,
    digests,
    models: {},
    model_mismatch: 0,
  })

  it('prints a group as apb stats does and lists only nodes with more than one executor', () => {
    expect(digestText(group('1a2b3c4d5e6f', 2, 200))).toBe(
      'profile 1a2b3c4d5e6f (bundle 1a2b3c4d5e6fb): 2 runs, first pass 2/2 (100%), median 1.0 min, 200 tokens per visit',
    )
    expect(digestText(group('aaa', 1))).toBe('profile aaa (bundle aaab): 1 run, first pass 1/1 (100%), median 1.0 min')
    expect(digestText({ ...group('aaa', 1), executor: 'x1abc', ephemeral: true })).toBe(
      'profile aaa (bundle aaab, executor x1abc, override): 1 run, first pass 1/1 (100%), median 1.0 min',
    )
    const v = { nodes: [node('a', [group('aaa', 1), group('bbb', 1)]), node('b', [group('ccc', 2)])] }
    expect(executorGroups(v)).toEqual([
      { node: 'a', groups: [digestText(group('aaa', 1)), digestText(group('bbb', 1))] },
    ])
    expect(executorGroups({ nodes: [] })).toEqual([])
  })

  it('shows the groups in the card', () => {
    const v = version('1.0.0', { nodes: [node('a', [group('aaa', 1), group('bbb', 1)])] })
    const body = render(PlaybookStatsCard, {
      props: { report: { runs: 2, versions: [v] } as StatsReport },
    }).body
    expect(body).toContain('playbook-stats-executors')
    expect(body).toContain('profile bbb (bundle bbbb)')
  })
})

describe('quality in the stats', () => {
  const row = (extra: Partial<QualityStats> = {}): QualityStats => ({
    criterion: 'outcome',
    source: 'builtin',
    grader: 'rules/1',
    task_type: 'default',
    digest: 'sha256:aa',
    graded_runs: 3,
    tally: { good: 2, partial: 0, poor: 1, not_applicable: 0, not_enough_data: 0 },
    good: { count: 2, of: 3, rate: 0.6667 },
    poor: { count: 1, of: 3, rate: 0.3333 },
    ...extra,
  })

  it('splits a node criterion by executor only when the row carries the breakdown', () => {
    expect(qualityExecutorTexts(row())).toEqual([])
    const split = row({
      by_executor: [
        { executor: 'x1', good: { count: 2, of: 2, rate: 1 }, poor: { count: 0, of: 2, rate: 0 }, graded_runs: 2 },
        { executor: 'x2', good: { count: 0, of: 1, rate: 0 }, poor: { count: 1, of: 1, rate: 1 }, graded_runs: 1 },
      ],
    })
    expect(qualityExecutorTexts(split)).toEqual([
      'executor x1: good 2/2 (100%), poor 0/2 (0%) over 2 graded runs',
      'executor x2: good 0/1 (0%), poor 1/1 (100%) over 1 graded run',
    ])
    const body = render(PlaybookStatsCard, {
      props: { report: { runs: 3, versions: [version('1.0.0', { quality: [split] })] } as StatsReport },
    }).body
    expect(body).toContain('executor x2: good 0/1 (0%)')
  })

  it('names a row by its key and prints it as apb stats does', () => {
    expect(qualityLabel(row())).toBe('outcome (rules/1, default)')
    expect(qualityText(row())).toBe('good 2/3 (67%), poor 1/3 (33%) over 3 graded runs')
    const one = row({
      graded_runs: 1,
      tally: { good: 0, partial: 0, poor: 0, not_applicable: 1, not_enough_data: 0 },
      good: { count: 0, of: 0 },
      poor: { count: 0, of: 0 },
    })
    expect(qualityText(one)).toBe('good 0/0, poor 0/0 over 1 graded run, 1 not applicable')
  })

  it('prints a delta in points and picks the version to compare with', () => {
    expect(qualityDeltaText({ criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa', good_delta: -0.5, poor_delta: 0.5 })).toBe(
      'good -50 points, poor +50 points',
    )
    expect(qualityDeltaText({ criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa' })).toBe(
      'good n/a, poor n/a',
    )
    // The sign follows the rounded points.
    const tiny = (good_delta: number) => ({ criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa', good_delta })
    expect(qualityDeltaText(tiny(-0.004))).toBe('good +0 points, poor n/a')
    expect(qualityDeltaText(tiny(0.004))).toBe('good +0 points, poor n/a')
    expect(qualityDeltaText(tiny(-0.006))).toBe('good -1 points, poor n/a')
    expect(previousVersion(report)).toBe('1.0.0')
    expect(previousVersion({ runs: 1, versions: [version('1.0.0')] })).toBe('')
    expect(previousVersion(null)).toBe('')
    expect(qualityDeltas(undefined)).toEqual([])
    const moved = { criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa', good_delta: 0 }
    const none = { criterion: 'consent', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa' }
    expect(qualityDeltas({ playbook: 'p', base: '1.0.0', quality: [moved, none] })).toEqual([moved])
  })

  it('shows the rows per version and the deltas of a comparison in the card', () => {
    const graded: StatsReport = {
      runs: 6,
      versions: [version('1.0.0', { quality: [row()] }), version('1.1.0', { quality: [row({ good: { count: 3, of: 3, rate: 1 } })] })],
      compare: {
        playbook: 'p',
        base: '1.0.0',
        against: '1.1.0',
        quality: [{ criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest: 'sha256:aa', good_delta: 0.33, poor_delta: -0.33 }],
      },
    }
    const body = render(PlaybookStatsCard, { props: { report: graded } }).body
    expect(body.match(/data-testid="playbook-stats-quality"/g)?.length).toBe(2)
    expect(body).toContain('quality outcome (rules/1, default):')
    expect(body).toContain('good 3/3 (100%)')
    expect(body).toContain('playbook-stats-quality-deltas')
    expect(body).toContain('quality, 1.1.0 against 1.0.0')
    expect(body).toContain('good +33 points, poor -33 points')
    // Nothing graded: no quality rows and no deltas.
    const plain = render(PlaybookStatsCard, { props: { report } }).body
    expect(plain).not.toContain('playbook-stats-quality')
  })

  it('tells apart one criterion graded under two definitions', () => {
    const delta = (digest: string) => ({ criterion: 'outcome', grader: 'rules/1', task_type: 'default', digest, good_delta: 0.1 })
    const twins: StatsReport = {
      runs: 6,
      versions: [
        version('1.0.0', { quality: [row(), row({ digest: 'sha256:bbbbbbbbbbbb' })] }),
        version('1.1.0', { quality: [row()] }),
      ],
      compare: { playbook: 'p', base: '1.0.0', against: '1.1.0', quality: [delta('sha256:aa'), delta('sha256:bbbbbbbbbbbb')] },
    }
    const body = render(PlaybookStatsCard, { props: { report: twins } }).body
    expect(body).toContain('outcome (rules/1, default), definition aa')
    expect(body).toContain('outcome (rules/1, default), definition bbbbbbbb')
    expect(qualityLabels([row()])).toEqual(['outcome (rules/1, default)'])
  })
})

describe('node statistics', () => {
  const stats = (node: string, over: Partial<NodeStats> = {}): NodeStats => ({
    node,
    runs: 0,
    in_runs: 0,
    in_runs_unknown: 0,
    not_run: [],
    executions: 0,
    completed: 0,
    open: 0,
    failed: 0,
    cancelled: 0,
    first_pass: { count: 0, of: 0 },
    retries: 0,
    fallbacks: 0,
    reentries: 0,
    duration: { count: 0 },
    deliverable_missing: 0,
    output_fields_missing: 0,
    digests: [],
    models: {},
    model_mismatch: 0,
    ...over,
  })

  it('shows a zero row for a node no run executed, with its reasons by class', () => {
    const n = stats('deploy', {
      in_runs: 4,
      not_run: [
        { class: 'observed', code: 'routed_elsewhere', runs: 2 },
        { class: 'inferred', code: 'condition_not_matched', runs: 1 },
        { class: 'unknown', code: 'cannot_tell', runs: 1 },
      ],
    })
    expect(nodeStatsText(n)).toBe('0 executions, 0 completed, 0 open, 0 failed; in 0 of 4 runs')
    expect(reasonClassText(n.not_run)).toBe('seen 2, inferred 1, unknown 1')
    expect(nodeLayer(n).text).toBe('not executed in 4 runs (seen 2, inferred 1, unknown 1)')
    expect(nodeLayer(n).text).not.toMatch(/dead/)
    expect(reasonLines(n.not_run)[0]).toBe('routed elsewhere: 2 (seen)')
  })

  it('counts a promptless finish as an execution and keeps race losers apart', () => {
    const finish = stats('done', { runs: 3, in_runs: 3, executions: 3, completed: 3 })
    expect(nodeStatsText(finish)).toBe('3 executions, 3 completed, 0 open, 0 failed; in 3 of 3 runs')
    const raced = stats('b', { runs: 1, in_runs: 2, executions: 1, completed: 1, cancelled: 1 })
    expect(nodeStatsText(raced)).toContain('1 cancelled by a race')
  })

  it('uses the membership denominator and shows unknown membership apart', () => {
    const n = stats('fix', {
      runs: 2,
      in_runs: 5,
      in_runs_unknown: 1,
      executions: 3,
      completed: 2,
      open: 1,
      failed: 1,
      not_run: [{ class: 'observed', code: 'routed_elsewhere', runs: 3 }],
    })
    expect(nodeLayer(n).text).toBe(
      'executed 3 times in 2 of 5 runs; not executed in 3 runs (seen 3); membership unknown in 1 run',
    )
    expect(nodeLayer(n).title).toContain('membership unknown in 1 run')
    expect(nodeStatsText(n)).toBe('3 executions, 2 completed, 1 open, 1 failed; in 2 of 5 runs, membership unknown in 1 run')
    const report = { runs: 5, versions: [version('1.0.0', { nodes: [n] })] } as StatsReport
    expect(nodeStatsOf(report, '1.0.0').get('fix')).toBe(n)
    const body = render(PlaybookStatsCard, { props: { report } }).body
    expect(body).toContain('data-testid="playbook-stats-nodes"')
    expect(body).toContain('not run: seen 3')
  })
})

describe('executor report', () => {
  const step = (over: Partial<StepStats> = {}): StepStats => ({
    binding: 'aaaaaaaaaaaa',
    step: 0,
    source: 'primary',
    executed_by: 'cli',
    attempts: 3,
    successes: { count: 1, of: 3, rate: 0.3333 },
    median_attempt_ms: 60_000,
    cost_known: { count: 0, of: 3 },
    reported_models: {},
    closes_by_reason: { check_rejected: 2 },
    ...over,
  })
  const group = (over: Partial<ExecutorGroup> = {}): ExecutorGroup => ({
    profile: 'p1',
    bundle: 'b1',
    executor: 'x1',
    ephemeral: false,
    runs: 3,
    attempts: 5,
    steps: [step(), step({ step: 1, source: 'fallback', attempts: 2, successes: { count: 2, of: 2, rate: 1 }, closes_by_reason: {}, cost_usd: 0.0123, cost_known: { count: 2, of: 2, rate: 1 } })],
    transitions: [
      { from_step: 0, to_step: 1, reason: 'check_rejected', count: 1 },
      { from_step: 0, to_step: 1, reason: 'not_recorded', count: 1 },
    ],
    ...over,
  })

  it('words a group, its steps and transitions as apb stats does', () => {
    expect(groupHeadText(group())).toBe('executor x1 (profile p1, bundle b1): 3 runs, 5 attempts')
    expect(groupHeadText(group({ ephemeral: true }))).toBe('executor x1, override (profile p1, bundle b1): 3 runs, 5 attempts')
    expect(stepLabelText(step())).toBe('step 0 primary (cli)')
    expect(stepLabelText(step({ step: undefined, source: 'not_recorded' }))).toBe('position not recorded (cli)')
    expect(stepText(step())).toBe('3 attempts, succeeded 1/3 (33%), median 1.0 min, closes: check rejected x2')
    expect(stepText(group().steps[1])).toBe('2 attempts, succeeded 2/2 (100%), median 1.0 min, cost $0.0123 over 2/2 (100%)')
    expect(stepText(step({ attempts: 0 }))).toBe('never ran')
    expect(stepText(step({ reported_models: { 'one:opus': 2, partial: 1 }, closes_by_reason: {} }))).toBe(
      '3 attempts, succeeded 1/3 (33%), median 1.0 min, models: opus x2, partial x1',
    )
    expect(transitionText(group().transitions[0])).toBe('step 0 to step 1: check rejected x1')
    expect(transitionText({ reason: 'not_recorded', count: 3 })).toBe('step ? to step ?: not recorded x3')
    expect(closeReasonWord('supervisor_interrupted')).toBe('supervisor interrupted')
    expect(hintFollowText({ hinted: 3, followed: 1, not_followed: 2, not_reported: 0 })).toBe(
      'hints: 3 hinted, 1 followed, 2 not followed, 0 not reported',
    )
  })

  it('lists a node only for groups with more than one step, a transition or a hint line', () => {
    const base = { runs: 1, in_runs: 1, in_runs_unknown: 0, not_run: [], executions: 1, completed: 1, open: 0, failed: 0, cancelled: 0, first_pass: { count: 1, of: 1 }, retries: 0, fallbacks: 0, reentries: 0, duration: { count: 0 }, deliverable_missing: 0, output_fields_missing: 0, digests: [], models: {}, model_mismatch: 0 }
    const single = group({ steps: [step()], transitions: [] })
    const hosted = group({ steps: [step({ executed_by: 'host' })], transitions: [], hint_follow: { hinted: 1, followed: 1, not_followed: 0, not_reported: 0 } })
    const v = {
      nodes: [
        { ...base, node: 'a', executors: [group(), single] },
        { ...base, node: 'b', executors: [single] },
        { ...base, node: 'c', executors: [hosted] },
        { ...base, node: 'd' },
      ],
    }
    expect(executorReports(v)).toEqual([
      { node: 'a', groups: [group()] },
      { node: 'c', groups: [hosted] },
    ])
  })

  it('shows the executor report in the card', () => {
    const n = { node: 'a', runs: 1, in_runs: 1, in_runs_unknown: 0, not_run: [], executions: 1, completed: 1, open: 0, failed: 0, cancelled: 0, first_pass: { count: 1, of: 1 }, retries: 0, fallbacks: 0, reentries: 0, duration: { count: 0 }, deliverable_missing: 0, output_fields_missing: 0, digests: [], models: {}, model_mismatch: 0, executors: [group()] }
    const body = render(PlaybookStatsCard, { props: { report: { runs: 1, versions: [version('1.0.0', { nodes: [n] })] } as StatsReport } }).body
    expect(body).toContain('playbook-stats-executor-report')
    expect(body).toContain('step 0 to step 1: not recorded x1')
  })
})

describe('interventions', () => {
  const k = (value: number, known_runs: number, runs: number) => ({ value, known_runs, runs })

  it('a count known in every run is the bare number, otherwise it says where', () => {
    expect(knownCountText(k(3, 3, 3))).toBe('3')
    expect(knownCountText(k(3, 2, 3))).toBe('3 (known in 2 of 3 runs)')
  })

  it('a category names its denominator and only the parts that say something', () => {
    const text = categoryText({
      category: 'review',
      unit: 'gate visits',
      denominator: 4,
      count: k(2, 3, 3),
      split: { channel: k(2, 3, 3), withdrawn: k(0, 3, 3), 'via:terminal': k(1, 2, 3) },
    })
    expect(text).toBe('2 over 4 gate visits (channel 2, via terminal 1 (known in 2 of 3 runs))')
  })

  it('the headline and the runs without any', () => {
    const s = {
      runs: 2,
      total: k(1, 1, 2),
      without_any: k(0, 1, 2),
      automatic_total: k(0, 2, 2),
      categories: [],
      engine: {},
      other: {},
      actor_unknown: {},
    }
    expect(interventionsHeadText(s)).toBe('1 (known in 1 of 2 runs) over 2 finished runs')
    expect(withoutAnyText(s)).toBe('0 of 1 (known in 1 of 2 runs)')
    expect(kindsText({})).toBeNull()
    expect(kindsText({ pause: 1 })).toBe('pause 1')
  })

  it('rounds the per run mean to two figures', () => {
    const s = {
      runs: 3,
      total: k(4, 3, 3),
      without_any: k(0, 3, 3),
      automatic_total: k(0, 3, 3),
      categories: [],
      engine: {},
      other: {},
      actor_unknown: {},
      per_run_median: 1,
      per_run_mean: 4 / 3,
    }
    expect(interventionsHeadText(s)).toContain('mean 1.33 per run')
  })

  it('says partly known when any denominator or split entry misses runs', () => {
    const full = (v: number): KnownCount => ({ value: v, known_runs: 2, runs: 2 })
    const base = {
      runs: 2,
      total: full(3),
      without_any: full(0),
      automatic_total: full(1),
      categories: [
        { category: 'review', unit: 'gate visits', denominator: 2, count: full(2), split: { channel: full(2) } },
      ],
      engine: {},
      other: {},
      actor_unknown: {},
    }
    expect(interventionsPartlyKnown(base)).toBe(false)
    expect(interventionsPartlyKnown({ ...base, without_any: { value: 0, known_runs: 1, runs: 2 } })).toBe(true)
    expect(interventionsPartlyKnown({ ...base, automatic_total: { value: 1, known_runs: 1, runs: 2 } })).toBe(true)
    expect(
      interventionsPartlyKnown({
        ...base,
        categories: [{ ...base.categories[0], split: { channel: { value: 2, known_runs: 1, runs: 2 } } }],
      }),
    ).toBe(true)
  })

  it('maps raw slugs to readable words and keeps the automatic qualifier', () => {
    expect(kindsText({ not_recorded: 1, infra_retry: 2 })).toBe('not recorded 1, infra retry 2')
    expect(categoryLabel('automatic')).toBe('automatic decisions applied (not interventions)')
    expect(categoryLabel('operator')).toBe('operator actions')
  })

  it('replaces every colon in a split name', () => {
    const text = categoryText({
      category: 'review',
      unit: 'gate visits',
      denominator: 3,
      count: k(3, 3, 3),
      split: { 'via:terminal:cli': k(3, 3, 3) },
    })
    expect(text).toContain('via terminal cli 3')
  })
})
