import { describe, expect, it } from 'vitest'
import {
  errorRate,
  formatDurationMs,
  outcomeSummary,
  usageCardState,
  type ConnectorFunctionStat,
  type ConnectorStats,
} from './connectorstats'

describe('errorRate', () => {
  const stat = (calls: number, errors: number): ConnectorFunctionStat => ({
    function: 'f',
    account: 'a',
    calls,
    errors,
    avgDurationMs: 0,
  })

  it('0/0 reads as 0%, never NaN', () => {
    expect(errorRate(stat(0, 0))).toBe('0%')
  })
  it('rounds to the nearest percent', () => {
    expect(errorRate(stat(3, 1))).toBe('33%')
  })
  it('all errors is 100%', () => {
    expect(errorRate(stat(2, 2))).toBe('100%')
  })
  it('no errors is 0%', () => {
    expect(errorRate(stat(5, 0))).toBe('0%')
  })
})

describe('outcomeSummary', () => {
  it('empty map -> "no recorded calls"', () => {
    expect(outcomeSummary({})).toBe('no recorded calls')
  })
  it('zero-count entries are dropped', () => {
    expect(outcomeSummary({ ok: 0, auth: 0 })).toBe('no recorded calls')
  })
  it('"ok" always sorts first', () => {
    expect(outcomeSummary({ auth: 1, ok: 5, network: 2 })).toBe('ok: 5, auth: 1, network: 2')
  })
  it('non-ok outcomes sort alphabetically', () => {
    expect(outcomeSummary({ timeout: 1, auth: 2 })).toBe('auth: 2, timeout: 1')
  })
})

describe('formatDurationMs', () => {
  it('rounds to the nearest millisecond', () => {
    expect(formatDurationMs(12.6)).toBe('13 ms')
  })
  it('handles zero', () => {
    expect(formatDurationMs(0)).toBe('0 ms')
  })
  it('non-finite input renders a dash rather than crashing', () => {
    expect(formatDurationMs(Number.NaN)).toBe('-')
  })
})

describe('usageCardState', () => {
  const stats = (over: Partial<ConnectorStats> = {}): ConnectorStats => ({
    connector: 'github',
    runsScanned: 5,
    calls: 0,
    byFunction: [],
    byOutcome: {},
    ...over,
  })

  it('installed and empty still renders: the section fills as playbooks call it', () => {
    expect(usageCardState(true, true, false, stats())).toBe('empty')
    expect(usageCardState(true, true, false, null)).toBe('empty')
  })
  it('installed and still loading shows a skeleton, since the card renders either way', () => {
    expect(usageCardState(true, false, false, null)).toBe('loading')
  })
  it('not installed and empty is hidden: no call can be made or was ever made', () => {
    expect(usageCardState(false, true, false, stats())).toBe('hidden')
    expect(usageCardState(false, true, false, null)).toBe('hidden')
  })
  it('not installed and loading stays hidden rather than flashing a skeleton', () => {
    expect(usageCardState(false, false, false, null)).toBe('hidden')
  })
  it('a failed request is not the same answer as no calls, in either state', () => {
    expect(usageCardState(true, true, true, null)).toBe('error')
    expect(usageCardState(false, true, true, null)).toBe('error')
  })
  it('real call history shows, uninstalled or not (event logs survive uninstall)', () => {
    expect(usageCardState(true, true, false, stats({ calls: 3 }))).toBe('data')
    expect(usageCardState(false, true, false, stats({ calls: 3 }))).toBe('data')
  })
})
