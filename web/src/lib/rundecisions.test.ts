import { describe, expect, it } from 'vitest'
import { render } from 'svelte/server'
import DecisionsPanel from './DecisionsPanel.svelte'
import { runEventJournal } from './journal'
import { decisionRows, decisionsSummary, decisionUseLines } from './rundecisions'
import type { RunDecisions } from './api.gen'
import type { WfEvent } from './types'

const totals: RunDecisions = {
  decisions: 3,
  requests: 2,
  replayed: 1,
  errors: 1,
  cost_usd: 0.0004,
  cost_estimated: true,
  p50_latency_ms: 190,
  p95_latency_ms: 250,
  by_use: { completion_check: { requests: 2, errors: 1, applied: 0, shadow_would_change: 2 } },
}

// Shapes mirror `decision_made` in crates/apb-engine/src/event.rs.
const events: WfEvent[] = [
  { seq: 0, ts: 1, type: 'run_started', playbook: 'demo', version: '1.0.0' },
  {
    seq: 1,
    ts: 2,
    type: 'decision_made',
    use_site: 'completion_check',
    node: 'fix',
    attempt: 1,
    provider: 'main',
    model: 'jev-1.13.0',
    mode: 'shadow',
    answers: { completion: { value: 'partial', p: 0.83, confidence: 0.7 }, final_result: { p: 0.1 } },
    applied: false,
    would_change: true,
    latency_ms: 190,
    cached: false,
    error: null,
  },
  { seq: 2, ts: 3, type: 'decision_made', use_site: 'completion_check', node: 'fix', provider: 'main', model: 'jev-1.13.0', mode: 'shadow', answers: { final_result: { p: 0.9 } }, latency_ms: 0, cached: true, error: null },
  { seq: 3, ts: 4, type: 'decision_made', use_site: 'completion_check', node: 'pr', provider: 'main', mode: 'shadow', answers: {}, latency_ms: 3000, cached: false, error: 'timeout' },
  { seq: 4, ts: 5, type: 'decision_made', use_site: 'judge_node', node: 'gate', provider: 'main', model: 'm', mode: 'enforce', applied: true, answers: { verdict: { value: 'pass', p: 0.97 } }, latency_ms: 120 },
]

describe('decisionsSummary', () => {
  it('names the totals, the cost and the latencies', () => {
    expect(decisionsSummary(totals)).toBe(
      '3 decisions (1 replayed, 1 error) · $0.0004 estimated · p50 190 ms · p95 250 ms · shadow would change 2',
    )
  })

  it('leaves out what is zero or absent', () => {
    const { p50_latency_ms: _a, p95_latency_ms: _b, ...rest } = totals
    const quiet: RunDecisions = {
      ...rest,
      decisions: 1,
      replayed: 0,
      errors: 0,
      cost_estimated: false,
      by_use: { completion_check: { requests: 1, errors: 0, applied: 0, shadow_would_change: 0 } },
    }
    expect(decisionsSummary(quiet)).toBe('1 decision · $0.0004')
    expect(decisionUseLines(quiet)).toEqual([{ use: 'completion_check', text: '1 request · 0 applied' }])
  })
})

describe('decisionRows', () => {
  it('reads one row per decision_made with its answer, source, latency and outcome', () => {
    const rows = decisionRows(events)
    expect(rows).toHaveLength(4)
    expect(rows[0]).toEqual({
      seq: 1,
      use: 'completion_check',
      node: 'fix',
      answer: 'completion partial (p=0.83), final_result p=0.10',
      source: 'main/jev-1.13.0',
      latency: '190 ms',
      outcome: 'shadow, would change',
    })
    expect(rows[1]).toMatchObject({ latency: 'cached', outcome: 'shadow' })
    expect(rows[2]).toMatchObject({ answer: 'error: timeout', source: 'main', outcome: 'error' })
    expect(rows[3]).toMatchObject({ answer: 'verdict pass (p=0.97)', outcome: 'applied' })
  })

  it('survives a shape a newer apb wrote', () => {
    const odd: WfEvent[] = [{ seq: 9, ts: 1, type: 'decision_made', answers: ['newer'], latency_ms: 'soon' }]
    expect(decisionRows(odd)).toEqual([
      { seq: 9, use: 'decision', node: null, answer: '', source: 'no provider', latency: '', outcome: 'not applied' },
    ])
  })
})

describe('the event journal', () => {
  it('gives decision_made a note and leaves every other row as it was', () => {
    const journal = runEventJournal(events)
    expect(journal).toHaveLength(events.length)
    expect(journal[0]).toEqual({ seq: 0, type: 'run_started', node: null, note: undefined })
    expect(journal[1].note).toBe(
      'completion_check · completion partial (p=0.83), final_result p=0.10 · main/jev-1.13.0 · 190 ms · shadow, would change',
    )
    expect(journal[3].note).toBe('completion_check · error: timeout · main · 3000 ms · error')
  })
})

describe('DecisionsPanel', () => {
  it('renders nothing for a run without decisions', () => {
    const { body } = render(DecisionsPanel, { props: { decisions: undefined, events: events.slice(0, 1) } })
    expect(body).not.toContain('Decisions')
    expect(body).not.toContain('run-decisions')
    const nullBody = render(DecisionsPanel, { props: { decisions: null, events } }).body
    expect(nullBody).not.toContain('run-decisions')
  })

  it('renders the totals, a line per use and a row per decision', () => {
    const { body } = render(DecisionsPanel, { props: { decisions: totals, events } })
    expect(body).toContain('Decisions')
    expect(body).toContain('3 decisions (1 replayed, 1 error)')
    expect(body).toContain('completion_check')
    expect(body).toContain('2 requests · 1 error · 0 applied · 2 would change')
    expect(body.match(/data-testid="run-decision-row"/g)).toHaveLength(4)
    expect(body).toContain('main/jev-1.13.0 · 190 ms')
  })
})
