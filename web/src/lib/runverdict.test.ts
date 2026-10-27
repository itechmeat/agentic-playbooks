import { describe, expect, it } from 'vitest'
import type { OutcomeSummary } from './api.gen'
import { evidenceTone, gapIsWarning, gapLabel, goalReachedText, needsLookCounts, resultText } from './runverdict'

const outcome: OutcomeSummary = {
  headline: 'Run succeeded; 1 of 2 checks passed; 3 not verified; needs a look (1 finding)',
  needs_look: { reasons: ['findings'], regressions: 0, findings: 1, warnings: 0 },
  done: ['succeeded: fix', 'twenty.update_opportunity: 3 writes went through'],
  verified: [
    { source: 'goal', subject: 'the fix script passes', result: 'passed' },
    { source: 'baseline', subject: 'unowned/count', result: 'not_met', detail: '37 -> 20, equals 0' },
  ],
  unverified: [
    { kind: 'manual_criterion', subject: 'a person spot-checks five deals' },
    { kind: 'self_reported_node', subject: 'fix', detail: "the agent's own report is the only evidence" },
    { kind: 'inbox_gap', subject: 'twenty/default: unread events 3-4 were dropped by inbox retention before they were read (found by fix)' },
  ],
  findings: [
    { node: 'fix', attempt: 2, kind: 'data_quality', location: 'deals:17', evidence: 'two deals share one id', why_not_fixed: 'needs a person' },
  ],
  warnings: [],
  baseline: { problem: ['Open deals without an owner: count 37'], change: ['unowned/count: 37 -> 20, equals 0: not met'] },
  agent_summary: 'All deals are assigned.',
}

describe('verdict wording', () => {
  it('tells a held check from one that did not hold', () => {
    expect(outcome.verified.map(evidenceTone)).toEqual(['pass', 'fail'])
    expect(resultText('not_met')).toBe('not met')
  })

  it('names why something is not verified, and warns about what the run missed', () => {
    expect(outcome.unverified.map(gapLabel)).toEqual(['to confirm by hand', 'self-reported', 'inbox gap'])
    expect(outcome.unverified.map(gapIsWarning)).toEqual([false, false, true])
  })

  it('counts what calls for a look, one or many of each', () => {
    expect(needsLookCounts({ reasons: ['regression', 'warnings'], regressions: 2, findings: 0, warnings: 1 })).toBe(
      '2 regressions, 1 warning',
    )
    expect(needsLookCounts({ reasons: ['regression', 'findings'], regressions: 1, findings: 3, warnings: 0 })).toBe(
      '1 regression, 3 findings',
    )
    expect(
      needsLookCounts({ reasons: ['findings', 'unknown_sends'], regressions: 0, findings: 1, warnings: 0, unknown_sends: 2 }),
    ).toBe('1 finding, 2 unknown sends')
    expect(needsLookCounts({ reasons: ['unknown_sends'], regressions: 0, findings: 0, warnings: 0, unknown_sends: 1 })).toBe(
      '1 unknown send',
    )
    expect(needsLookCounts({ reasons: ['list_items'], regressions: 0, findings: 0, warnings: 0, list_items: 2 })).toBe(
      '2 list items',
    )
  })

  it('has no count text when nothing calls for a look', () => {
    expect(needsLookCounts(undefined)).toBeNull()
    expect(needsLookCounts(null)).toBeNull()
    expect(needsLookCounts({ reasons: [], regressions: 0, findings: 0, warnings: 0 })).toBeNull()
  })

  it('says when the goal cannot tell', () => {
    expect([true, false, undefined].map(goalReachedText)).toEqual(['yes', 'no', 'not known'])
  })
})

describe('RunVerdictPanel', () => {
  const html = async (o: OutcomeSummary) => {
    const { render } = await import('svelte/server')
    const Panel = (await import('./RunVerdictPanel.svelte')).default
    return render(Panel, { props: { outcome: o } }).body
  }

  it('keeps verified and not verified apart and tells the baseline as problem, change, goal', async () => {
    const body = await html(outcome)
    expect(body).toContain(outcome.headline)
    expect(body).toContain('Needs a look: 1 finding')
    expect(body.match(/data-testid="run-verdict-evidence"/g)?.length).toBe(2)
    expect(body.match(/data-testid="run-verdict-gap"/g)?.length).toBe(3)
    // The inbox gap lives in the Not verified list now.
    expect(body).toContain('data-kind="inbox_gap"')
    expect(body).toContain('deals:17')
    expect(body).toContain('Open deals without an owner: count 37')
    expect(body).toContain('not known')
    expect(body).toContain("Agent's own summary, not checked")
  })

  it('says when the baseline has no comparison yet', async () => {
    const body = await html({ ...outcome, baseline: { problem: ['Open deals without an owner: count 37'], change: [] } })
    expect(body).toContain('no comparison yet')
  })

  it('shows only the headline for a run with nothing to tell', async () => {
    const body = await html({ headline: 'Run running; nothing checked by the engine', done: [], verified: [], unverified: [], findings: [], warnings: [] })
    expect(body).toContain('data-testid="run-verdict-headline"')
    for (const block of ['done', 'verified', 'unverified', 'findings', 'warnings', 'baseline', 'needs-look']) {
      expect(body).not.toContain(`data-testid="run-verdict-${block}"`)
    }
  })
})
