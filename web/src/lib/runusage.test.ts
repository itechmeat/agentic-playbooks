import { describe, expect, it } from 'vitest'
import { attemptUsageNote, formatCost, formatTokens, runUsageSummary, unknownEventsNote } from './runusage'
import type { RunUsage } from './api.gen'

const total: RunUsage = {
  attempts: 3,
  input_tokens: 1250,
  output_tokens: 820,
  cache_read_tokens: 1_480_000,
  cache_write_tokens: 0,
  cost_attempts: 3,
  cost_usd: 0.4312,
}

describe('formatTokens', () => {
  it('keeps small counts exact and abbreviates large ones', () => {
    expect([950, 1000, 12_340, 999_999, 1_200_000, 3_000_000].map(formatTokens)).toEqual([
      '950',
      '1k',
      '12.3k',
      '1M',
      '1.2M',
      '3M',
    ])
  })
})

describe('formatCost', () => {
  it('shows cheap attempts with four decimals and larger sums in cents', () => {
    expect(formatCost(0.0037)).toBe('$0.0037')
    expect(formatCost(12.345)).toBe('$12.35')
  })
})

describe('runUsageSummary', () => {
  it('lists the totals, skipping an empty cache write, and the reported cost', () => {
    expect(runUsageSummary(total)).toBe('1.3k in · 820 out · 1.5M cache read · 3 attempts · $0.4312 reported')
  })

  it('says when only some attempts reported a cost or when counts are estimates', () => {
    expect(runUsageSummary({ ...total, cost_attempts: 1, estimated: true })).toBe(
      "1.3k in · 820 out · 1.5M cache read · 3 attempts · $0.4312 reported by 1 of 3 · partly estimated by apb",
    )
  })

  it('shows no cost when no attempt reported one', () => {
    const { cost_usd: _, ...noCost } = total
    expect(runUsageSummary({ ...noCost, attempts: 1, cost_attempts: 0 })).toBe('1.3k in · 820 out · 1.5M cache read · 1 attempt')
  })
})

describe('attemptUsageNote', () => {
  it('renders an attempt_finished usage block', () => {
    expect(
      attemptUsageNote({ input_tokens: 18, output_tokens: 443, cache_read_tokens: 52079, cache_write_tokens: 14967, cost_usd: 0.0374, source: 'reported' }),
    ).toBe('tokens: 18 in, 443 out, 52.1k cache read, 15k cache write, $0.0374')
    expect(attemptUsageNote({ input_tokens: 5, output_tokens: 1, source: 'estimated' })).toBe('tokens: 5 in, 1 out, estimated')
  })

  it('is absent for an attempt without usage or with an unexpected shape', () => {
    expect(attemptUsageNote(undefined)).toBeUndefined()
    expect(attemptUsageNote({ tokens: 3 })).toBeUndefined()
  })
})

describe('unknownEventsNote', () => {
  it('names the count only when events were skipped', () => {
    expect(unknownEventsNote(0)).toBeUndefined()
    expect(unknownEventsNote(undefined)).toBeUndefined()
    expect(unknownEventsNote(1)).toBe('1 unknown event (newer apb?)')
    expect(unknownEventsNote(4)).toBe('4 unknown events (newer apb?)')
  })
})
