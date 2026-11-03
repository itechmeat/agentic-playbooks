// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import ScenarioRunPanel from './ScenarioRunPanel.svelte'
import type { PlaybookScenarios, ScenarioReport } from './api.gen'
import { auth } from './auth.svelte'
import { SCENARIO_MAX_DECISION_REQUESTS } from './consts.gen'

// The panel lists and runs scenarios over HTTP: the test stubs `fetch`
// and checks the requests it sends and what it shows of each answer.
const fetchMock = vi.fn<typeof fetch>()
let app: ReturnType<typeof mount> | null = null
beforeEach(() => vi.stubGlobal('fetch', fetchMock))
afterEach(() => {
  auth.set({ required: false, authenticated: true, checked: false })
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const list: PlaybookScenarios = {
  id: 'p',
  version: '1.0.0',
  playbook_digest: 'sha256:v1',
  scenarios: [
    {
      name: 'green',
      title: 'The happy path',
      digest: 'sha256:g',
      latest: [
        {
          scenario: 'green',
          level: 'quick',
          verdict: 'passed',
          version: '0.9.0',
          playbook_digest: 'sha256:v0',
          scenario_digest: 'sha256:g',
          at_ms: 1,
          fresh: false,
          run_id: 'sr-old',
        },
      ],
    },
    { name: 'pinned', digest: 'sha256:p', stale: 'pinned to another digest', latest: [] },
  ],
  issues: [{ code: 'V146', severity: 'error', message: 'broken.yaml does not parse', edges: [] }],
}

const ref = (node: string, visit = 1, attempt?: number) => ({ kind: 'agent_attempt' as const, node, visit, attempt })

const report: ScenarioReport = {
  run_id: 'sr-2',
  level: 'quick',
  scenario: 'green',
  scenario_digest: 'sha256:g',
  playbook: 'p',
  version: '1.0.0',
  playbook_digest: 'sha256:v1',
  verdict: 'failed',
  outcome: 'success',
  finish: 'done',
  checks: [
    { check: 'run.outcome', passed: true, expected: 'success', observed: 'success' },
    { check: 'route.path', passed: false, expected: 'start, work, done', observed: 'start, check, done' },
  ],
  executions: [{ node: 'work', visit: 1, status: 'success', attempts: [{ n: 1, status: 'success', answer: 'used' }] }],
  routes: [],
  edges_taken: [{ edge: 'e:start>work#0', count: 1 }],
  joins_dead: [],
  templates_empty: [{ node: 'work', visit: 1, reference: 'nodes.clarify.review_note', why: 'never ran' }],
  answers: { used: [ref('work')], defaults: [], missing: [ref('check')], extra: [ref('work', 1, 2)] },
  boundaries: [],
  decision_requests: 0,
}

const q = (id: string) => document.querySelector(`[data-testid="${id}"]`) as HTMLElement | null
const all = (id: string) => [...document.querySelectorAll(`[data-testid="${id}"]`)] as HTMLElement[]

async function settle() {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve()
    await tick()
  }
  flushSync()
}

async function render() {
  fetchMock.mockResolvedValueOnce(json(list))
  app = mount(ScenarioRunPanel, { target: document.body, props: { id: 'p', workspace: 'w1' } })
  await settle()
}

describe('ScenarioRunPanel', () => {
  it('lists the scenarios with their newest result, stale ones marked, and the files that did not load', async () => {
    await render()
    expect(fetchMock).toHaveBeenLastCalledWith('/api/playbooks/p/scenarios?workspace=w1', expect.anything())
    const rows = all('scenario-row')
    expect(rows.map((r) => r.dataset.name)).toEqual(['green', 'pinned'])
    expect(rows[0].textContent).toContain('The happy path')
    expect(q('scenario-latest')!.textContent).toContain('passed')
    expect(q('scenario-latest')!.textContent).toContain('stale')
    expect(rows[1].textContent).toContain('pinned to another digest')
    expect(q('scenario-issue')!.textContent).toContain('broken.yaml does not parse')
  })

  it('runs a quick scenario and shows the failed checks first, the trace and the answers nobody used', async () => {
    await render()
    fetchMock.mockResolvedValueOnce(json({ report }))
    fetchMock.mockResolvedValueOnce(json(list))
    all('scenario-run')[0].click()
    await settle()
    expect(fetchMock.mock.calls[1][0]).toBe('/api/playbooks/p/scenario-runs?workspace=w1')
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({ scenario: 'green', level: 'quick' })
    expect(q('scenario-verdict')!.textContent).toContain('failed')
    const checks = all('scenario-check')
    expect(checks.map((c) => c.dataset.passed)).toEqual(['false', 'true'])
    expect(checks[0].textContent).toContain('start, check, done')
    expect(q('scenario-execution')!.textContent).toContain('work')
    expect(q('scenario-unused')!.textContent).toContain('work')
    expect(q('scenario-unused')!.textContent).toContain('attempt 2')
    expect(q('scenario-missing')!.textContent).toContain('check')
    expect(q('scenario-empty')!.textContent).toContain('nodes.clarify.review_note')
  })

  it('asks for a request cap and a confirmation before a smoke run spends', async () => {
    await render()
    const level = q('scenario-level') as HTMLSelectElement
    level.value = 'smoke'
    level.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    all('scenario-run')[0].click()
    flushSync()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(q('scenario-smoke-confirm')!.textContent).toContain('3 decision requests')
    fetchMock.mockResolvedValueOnce(json({ report: { ...report, level: 'smoke', verdict: 'passed' } }))
    fetchMock.mockResolvedValueOnce(json(list))
    q('scenario-smoke-go')!.click()
    await settle()
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      scenario: 'green',
      level: 'smoke',
      max_decision_requests: 3,
    })
  })

  it('opens a stored result', async () => {
    await render()
    fetchMock.mockResolvedValueOnce(json({ report: { ...report, run_id: 'sr-old' } }))
    q('scenario-open')!.click()
    await settle()
    expect(fetchMock).toHaveBeenLastCalledWith('/api/playbooks/p/scenario-runs/sr-old?workspace=w1', expect.anything())
    expect(q('scenario-report')!.textContent).toContain('sr-old')
  })

  it('holds the cap to what the server takes and shows a refusal in the words of the server', async () => {
    await render()
    const level = q('scenario-level') as HTMLSelectElement
    level.value = 'smoke'
    level.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    expect((q('scenario-cap') as HTMLInputElement).max).toBe(String(SCENARIO_MAX_DECISION_REQUESTS))
    all('scenario-run')[0].click()
    flushSync()
    fetchMock.mockResolvedValueOnce(json({ error: 'a smoke scenario run is refused by a dashboard without keys' }, 409))
    fetchMock.mockResolvedValueOnce(json(list))
    q('scenario-smoke-go')!.click()
    await settle()
    const shown = q('scenario-error')!.textContent ?? ''
    expect(shown).toContain('The smoke run of green was refused: a smoke scenario run is refused by a dashboard without keys')
    expect(shown).not.toContain('http')
  })

  it('sends a cap the server takes when the field is emptied or over the limit', async () => {
    await render()
    const level = q('scenario-level') as HTMLSelectElement
    level.value = 'smoke'
    level.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    const cap = q('scenario-cap') as HTMLInputElement
    cap.value = ''
    cap.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    all('scenario-run')[0].click()
    flushSync()
    const asked = (q('scenario-smoke-confirm')!.textContent ?? '').replace(/\s+/g, ' ')
    expect(asked).not.toContain('NaN')
    expect(asked).toContain('at most 1 decision request')
    fetchMock.mockResolvedValueOnce(json({ report: { ...report, level: 'smoke', verdict: 'passed' } }))
    fetchMock.mockResolvedValueOnce(json(list))
    q('scenario-smoke-go')!.click()
    await settle()
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body)).max_decision_requests).toBe(1)

    cap.value = String(SCENARIO_MAX_DECISION_REQUESTS + 50)
    cap.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    all('scenario-run')[0].click()
    flushSync()
    fetchMock.mockResolvedValueOnce(json({ report: { ...report, level: 'smoke', verdict: 'passed' } }))
    fetchMock.mockResolvedValueOnce(json(list))
    q('scenario-smoke-go')!.click()
    await settle()
    expect(JSON.parse(String(fetchMock.mock.calls[3][1]?.body)).max_decision_requests).toBe(SCENARIO_MAX_DECISION_REQUESTS)
  })

  it('offers smoke only with keys: a dashboard without keys points to the command line', async () => {
    auth.set({ required: false, authenticated: true, checked: true })
    await render()
    const smoke = [...(q('scenario-level') as HTMLSelectElement).options].find((o) => o.value === 'smoke')!
    expect(smoke.disabled).toBe(true)
    expect(q('scenario-smoke-keyless')!.textContent).toContain('apb eval --level smoke')
    auth.set({ required: true, authenticated: true, checked: true })
    flushSync()
    expect(smoke.disabled).toBe(false)
    expect(q('scenario-smoke-keyless')).toBeNull()
  })
})
