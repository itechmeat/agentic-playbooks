// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import { SvelteMap } from 'svelte/reactivity'
import DecisionsPanel from './DecisionsPanel.svelte'
import { runEventJournal } from './journal'
import { decisionRows, decisionsSummary, decisionUseLines, exampleDraftOf } from './rundecisions'
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

// The panel mounted once, as markup.
function render(
  component: typeof DecisionsPanel,
  opts: { props: { decisions?: RunDecisions | null; events: WfEvent[] } },
) {
  const app = mount(component, { target: document.body, props: opts.props })
  flushSync()
  const body = document.body.innerHTML
  unmount(app)
  document.body.innerHTML = ''
  return { body }
}

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

describe('exampleDraftOf', () => {
  it('drafts an example of a judge execution with its questions, and nothing for other decisions', () => {
    const journal: WfEvent[] = [
      // The first execution of gate takes two attempts; the decision is in the second execution.
      { seq: 0, ts: 1, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 1, ts: 2, type: 'node_started', node: 'gate', attempt: 2 },
      { seq: 2, ts: 3, type: 'node_finished', node: 'gate', status: 'success' },
      { seq: 3, ts: 4, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 4, ts: 5, type: 'decision_made', use_site: 'judge_node', node: 'gate', answers: { verdict: { value: 'pass', p: 0.97 }, risky: { p: 0.2 } } },
      { seq: 5, ts: 6, type: 'node_finished', node: 'gate', status: 'success' },
    ]
    expect(exampleDraftOf(journal, 4)).toEqual({
      node: 'gate',
      execution: 2,
      questions: [
        { name: 'verdict', yesNo: false, observed: 'pass' },
        { name: 'risky', yesNo: true, observed: '' },
      ],
    })
    expect(exampleDraftOf(events, 1)).toBeNull()
    expect(exampleDraftOf(events, 3)).toBeNull()
  })

  it('leaves a score unfilled: the decision holds its expected level, an example names a level or band', () => {
    const journal: WfEvent[] = [
      { seq: 0, ts: 1, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 1, ts: 2, type: 'decision_made', use_site: 'judge_node', node: 'gate', answers: { effort: { value: 1.4, confidence: 0.7 } } },
    ]
    expect(exampleDraftOf(journal, 1)?.questions).toEqual([{ name: 'effort', yesNo: false, observed: '' }])
  })
})

describe('DecisionsPanel saving an example', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    document.body.innerHTML = ''
  })

  it('saves a judge decision with the expected answers typed, a yes or no as a boolean, and shows what was stored', async () => {
    const fetchMock = vi.fn<typeof fetch>()
    vi.stubGlobal('fetch', fetchMock)
    const journal: WfEvent[] = [
      { seq: 0, ts: 1, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 1, ts: 2, type: 'decision_made', use_site: 'judge_node', node: 'gate', answers: { verdict: { value: 'pass', p: 0.97 }, risky: { p: 0.2 } } },
    ]
    const app = mount(DecisionsPanel, {
      target: document.body,
      props: { decisions: totals, events: journal, runId: 'r1', workspace: 'w1' },
    })
    flushSync()
    ;(document.querySelector('[data-testid="decision-save-example"]') as HTMLButtonElement).click()
    flushSync()
    const verdict = document.querySelector('[data-testid="example-expected-verdict"]') as HTMLInputElement
    expect(verdict.value).toBe('pass')
    verdict.value = 'fail'
    verdict.dispatchEvent(new Event('input', { bubbles: true }))
    const risky = document.querySelector('[data-testid="example-expected-risky"]') as HTMLSelectElement
    risky.value = 'false'
    risky.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    const stored = { id: 'ex-1', node: 'gate', inputs: { instruction: 'Fix it', params: {}, outputs: {}, reviews: {} } }
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify(stored), { status: 200 }))
    ;(document.querySelector('[data-testid="example-save"]') as HTMLButtonElement).click()
    for (let i = 0; i < 4; i++) {
      await Promise.resolve()
      await tick()
    }
    expect(fetchMock.mock.lastCall?.[0]).toBe('/api/runs/r1/judge/gate/examples?workspace=w1')
    expect(JSON.parse(String(fetchMock.mock.lastCall?.[1]?.body))).toEqual({
      execution: 1,
      expected: { verdict: 'fail', risky: false },
    })
    expect(document.querySelector('[data-testid="example-saved"]')!.textContent).toContain('instruction: Fix it')
    unmount(app)
  })

  it('drops an open draft when the page moves to another run', () => {
    const journal: WfEvent[] = [
      { seq: 0, ts: 1, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 1, ts: 2, type: 'decision_made', use_site: 'judge_node', node: 'gate', answers: { verdict: { value: 'pass' } } },
    ]
    // The run page keeps this panel across runs and hands it the new id.
    const run = new SvelteMap([['id', 'r1']])
    const app = mount(DecisionsPanel, {
      target: document.body,
      props: {
        decisions: totals,
        events: journal,
        get runId() {
          return run.get('id')
        },
        workspace: 'w1',
      },
    })
    flushSync()
    ;(document.querySelector('[data-testid="decision-save-example"]') as HTMLButtonElement).click()
    flushSync()
    expect(document.querySelector('[data-testid="example-draft"]')).not.toBeNull()
    run.set('id', 'r2')
    flushSync()
    expect(document.querySelector('[data-testid="example-draft"]')).toBeNull()
    unmount(app)
  })
  it('keeps a late answer of a save on another run off this run', async () => {
    const fetchMock = vi.fn<typeof fetch>()
    vi.stubGlobal('fetch', fetchMock)
    let answer: (r: Response) => void = () => {}
    fetchMock.mockReturnValueOnce(new Promise<Response>((resolve) => (answer = resolve)))
    const journal: WfEvent[] = [
      { seq: 0, ts: 1, type: 'node_started', node: 'gate', attempt: 1 },
      { seq: 1, ts: 2, type: 'decision_made', use_site: 'judge_node', node: 'gate', answers: { verdict: { value: 'pass' } } },
    ]
    const run = new SvelteMap([['id', 'r1']])
    const app = mount(DecisionsPanel, {
      target: document.body,
      props: {
        decisions: totals,
        events: journal,
        get runId() {
          return run.get('id')
        },
        workspace: 'w1',
      },
    })
    const settle = async () => {
      for (let i = 0; i < 4; i++) {
        await Promise.resolve()
        await tick()
      }
    }
    flushSync()
    ;(document.querySelector('[data-testid="decision-save-example"]') as HTMLButtonElement).click()
    flushSync()
    ;(document.querySelector('[data-testid="example-save"]') as HTMLButtonElement).click()
    await settle()
    // The page moves to r2 and opens a draft there before r1's save answers.
    run.set('id', 'r2')
    flushSync()
    ;(document.querySelector('[data-testid="decision-save-example"]') as HTMLButtonElement).click()
    flushSync()
    expect(document.querySelector('[data-testid="example-draft"]')).not.toBeNull()
    const stored = { id: 'ex-1', node: 'gate', inputs: { instruction: 'Fix it', params: {}, outputs: {}, reviews: {} } }
    answer(new Response(JSON.stringify(stored), { status: 200 }))
    await settle()
    expect(document.querySelector('[data-testid="example-saved"]')).toBeNull()
    expect(document.querySelector('[data-testid="example-draft"]')).not.toBeNull()
    expect((document.querySelector('[data-testid="example-save"]') as HTMLButtonElement).disabled).toBe(false)
    unmount(app)
  })
})
