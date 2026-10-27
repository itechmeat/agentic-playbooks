import { describe, expect, it } from 'vitest'
import type { RunEstimate } from './api.gen'
import {
  budgetFromForm,
  comparableText,
  estimateRows,
  formFromBlock,
  needsPolicyChoice,
  parseTokens,
  parseUsd,
  tokensText,
  minutesText,
  unestimatedFor,
  unestimatedText,
} from './runestimate'

const estimate = (extra: Partial<RunEstimate> = {}): RunEstimate => ({
  playbook: 'p',
  version: '1.0.0',
  execution: 'cli',
  comparable: {
    runs: 7,
    succeeded: 6,
    window: 50,
    runs_without_usage: 1,
    attempts_without_usage: 3,
    attempts_without_cost: 21,
    profile_mismatch: { triage: 2 },
  },
  time: {
    basis: 'history',
    spread: { n: 6, p50: 840_000, low: 540_000, high: 1_260_000, kind: 'iqr' },
    human_wait_p50_ms: 120_000,
  },
  tokens: { basis: 'history', spread: { n: 5, p50: 182_000, low: 121_000, high: 260_000, kind: 'iqr' } },
  cost_usd: null,
  nodes: ['triage', 'review', 'fix'].map((node) => ({
    node,
    visits_p50: 1,
    per_visit: {},
    per_run: {},
    per_attempt_p50: {},
  })),
  unestimated: ['triage', 'fix'],
  unestimated_tokens: ['triage', 'fix'],
  unestimated_cost: ['triage', 'review', 'fix'],
  ...extra,
})

describe('runestimate', () => {
  it('prints the ranges and an honest unknown cost', () => {
    expect(estimateRows(estimate())).toEqual([
      { label: 'time', value: '~14 min (9 min-21 min), human waits ~2 min', known: true },
      { label: 'tokens', value: '~182k (121k-260k)', known: true },
      { label: 'cost', value: 'unknown: no comparable attempt reported a cost', known: false },
    ])
    const single = estimate({
      time: { basis: 'declared', spread: { n: 0, p50: 30_000, low: 30_000, high: 30_000, kind: 'single' } },
      tokens: null,
      cost_usd: { basis: 'history', spread: { n: 1, p50: 0.5, low: 0.5, high: 0.5, kind: 'single' } },
    })
    expect(estimateRows(single).map((r) => r.value)).toEqual([
      '~30 s (declared expected durations)',
      'unknown: no comparable attempt reported usage',
      '~$0.50',
    ])
  })

  it('counts the comparable runs and names why a node is not estimated', () => {
    expect(comparableText(estimate())).toBe('7 comparable runs (cli); 3 attempts without usage')
    expect(unestimatedText(estimate())).toEqual(['triage (no history with this profile)', 'fix (no history)'])
    // Under a dollar limit `review`, with token history only, is named too.
    expect(unestimatedText(estimate(), { usd: 2 })).toEqual([
      'triage (no history with this profile)',
      'review (no cost figure)',
      'fix (no history)',
    ])
    expect(unestimatedText(estimate(), { tokens: 1000 })).toEqual([
      'triage (no history with this profile)',
      'fix (no history)',
    ])
  })

  it('asks for a policy under ask per limited dimension, and when the estimate failed', () => {
    const tokens = { tokens: 1000 }
    expect(needsPolicyChoice(estimate(), 'ask', tokens)).toBe(true)
    expect(needsPolicyChoice(estimate(), 'continue', tokens)).toBe(false)
    expect(needsPolicyChoice(estimate(), 'ask', {})).toBe(false)
    // Every node has token history, but `review` has no cost history: a
    // dollar limit still needs the choice, a token limit does not.
    const usdGap = estimate({ unestimated: [], unestimated_tokens: [], unestimated_cost: ['review'] })
    expect(needsPolicyChoice(usdGap, 'ask', tokens)).toBe(false)
    expect(needsPolicyChoice(usdGap, 'ask', { usd: 2 })).toBe(true)
    expect(unestimatedFor(usdGap, { usd: 2 })).toEqual(['review'])
    expect(unestimatedFor(estimate(), { tokens: 1, usd: 2 })).toEqual(['triage', 'review', 'fix'])
    expect(needsPolicyChoice(null, 'ask', tokens)).toBe(false)
    expect(needsPolicyChoice(null, 'ask', tokens, true)).toBe(true)
  })

  it('picks the unit after rounding', () => {
    expect(tokensText(950)).toBe('950')
    expect(tokensText(999.6)).toBe('1k')
    expect(tokensText(1300)).toBe('1.3k')
    expect(tokensText(999_600)).toBe('1m')
    expect(minutesText(59_600)).toBe('1 min')
    expect(minutesText(40_000)).toBe('40 s')
  })

  it('parses amounts the way the CLI does', () => {
    expect(parseTokens('300k')).toBe(300_000)
    expect(parseTokens('1.5M')).toBe(1_500_000)
    expect(parseTokens('250_000')).toBe(250_000)
    expect(parseTokens('0')).toBeNull()
    expect(parseTokens('lots')).toBeNull()
    // A decimal comma would read ten times larger: refused, as by the CLI.
    expect(parseTokens('1,5m')).toBeNull()
    expect(parseUsd('$3.50')).toBe(3.5)
    expect(parseUsd('0')).toBeNull()
    expect(parseUsd('-1')).toBeNull()
  })

  it('sends only what differs from the playbook block', () => {
    const block = { tokens: 400_000, on_no_estimate: 'ask' as const }
    const form = formFromBlock(block)
    expect(form).toEqual({ tokens: '400000', usd: '', policy: 'ask' })
    expect(budgetFromForm(form, block)).toEqual({})
    expect(budgetFromForm({ ...form, policy: 'continue' }, block)).toEqual({
      budget: { on_no_estimate: 'continue' },
    })
    expect(budgetFromForm({ tokens: '300k', usd: '$2', policy: 'pause' }, null)).toEqual({
      budget: { tokens: 300_000, usd: 2, on_no_estimate: 'pause' },
    })
    expect(budgetFromForm({ tokens: '', usd: '', policy: 'continue' }, null)).toEqual({})
    expect(budgetFromForm({ tokens: 'x', usd: '', policy: 'ask' }, null).error).toContain('tokens')
    // An emptied input keeps the block's limit, so the chosen policy is sent.
    expect(budgetFromForm({ tokens: '', usd: '', policy: 'pause' }, block)).toEqual({
      budget: { on_no_estimate: 'pause' },
    })
  })
})
