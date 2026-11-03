// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import CandidateSummaryCard from './CandidateSummaryCard.svelte'
import type { CandidateSummary, SummaryItem } from './api.gen'

// The card reads the summary and promotes over HTTP, so the test stubs
// `fetch` and checks what the card sends and shows for each answer. Runes
// (a page that bumps `refresh`) need the `.svelte.` infix.
const fetchMock = vi.fn<typeof fetch>()
let app: ReturnType<typeof mount> | null = null
beforeEach(() => vi.stubGlobal('fetch', fetchMock))
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const attention: SummaryItem = { axis: 'definition', code: 'scenario_not_run', detail: 'scenario green has no result for 1.0.1' }
const fitnessItem: SummaryItem = { axis: 'fitness', code: 'fitness_problem', detail: 'profile main is missing here' }

function summary(over: Partial<CandidateSummary> = {}): CandidateSummary {
  return {
    summary_id: 'sum-1',
    playbook: 'p',
    candidate: { version: '1.0.1', digest: 'sha256:cand' },
    current: { version: '1.0.0', digest: 'sha256:cur' },
    computed_at_ms: 1,
    fitness: { state: 'attention', detail: { report: {} as never, start_actions: [] } },
    definition: {
      state: 'attention',
      detail: {
        issues: [],
        scenarios: [
          {
            scenario: 'red',
            level: 'quick',
            verdict: 'passed',
            version: '1.0.0',
            playbook_digest: 'sha256:old',
            scenario_digest: 'sha256:s',
            at_ms: 1,
            fresh: false,
            run_id: 'sr-1',
          },
        ],
        not_run: ['green'],
      },
    },
    quality: { state: 'unknown', detail: { view: { version: '1.0.1', current: '1.0.0', trials: 0 } as never } },
    blocking: [],
    attention: [attention, fitnessItem],
    ...over,
  }
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

async function render(first: CandidateSummary | null, extra: { frozen?: boolean } = {}) {
  fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: first }))
  const onDecided = vi.fn()
  app = mount(CandidateSummaryCard, { target: document.body, props: { id: 'p', workspace: 'w1', onDecided, ...extra } })
  await settle()
  return { onDecided }
}

const lastBody = () => JSON.parse(String(fetchMock.mock.lastCall?.[1]?.body))

describe('CandidateSummaryCard', () => {
  it('shows the three axes apart and promotes exactly what it showed once the attention items are acknowledged', async () => {
    const { onDecided } = await render(summary())
    expect(fetchMock).toHaveBeenLastCalledWith('/api/playbooks/p/candidate/summary?workspace=w1', expect.anything())
    const axes = all('summary-axis')
    expect(axes.map((a) => a.dataset.axis)).toEqual(['fitness', 'definition', 'quality'])
    expect(axes.map((a) => a.dataset.state)).toEqual(['attention', 'attention', 'unknown'])
    expect(q('summary-axis-fitness')!.textContent).toContain('on this machine')
    // A result for another digest reads stale; a scenario with none is listed.
    expect(q('summary-scenario')!.textContent).toContain('stale')
    expect(q('summary-not-run')!.textContent).toContain('green')
    expect(all('summary-attention-item').map((i) => i.textContent)).toEqual([
      expect.stringContaining(attention.detail),
      expect.stringContaining(fitnessItem.detail),
    ])

    // Promote asks first, naming each attention item.
    q('summary-promote')!.click()
    flushSync()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(q('summary-confirm')!.textContent).toContain(attention.detail)
    expect(q('summary-confirm')!.textContent).toContain(fitnessItem.detail)

    fetchMock.mockResolvedValueOnce(json({ verdict: 'promote', summary_id: 'sum-1' }))
    q('summary-confirm-promote')!.click()
    await settle()
    expect(fetchMock.mock.calls[1][0]).toBe('/api/playbooks/p/candidate/promote?workspace=w1')
    expect(lastBody()).toEqual({ expected_version: '1.0.1', expected_digest: 'sha256:cand', acknowledged: 'sum-1' })
    expect(onDecided).toHaveBeenCalledOnce()
  })

  it('promotes without asking and without an id when nothing needs attention', async () => {
    await render(summary({ attention: [] }))
    fetchMock.mockResolvedValueOnce(json({ verdict: 'promote' }))
    q('summary-promote')!.click()
    await settle()
    expect(lastBody()).toEqual({ expected_version: '1.0.1', expected_digest: 'sha256:cand' })
  })

  it('keeps promotion closed while an item blocks', async () => {
    const blocking: SummaryItem = { axis: 'definition', code: 'validator_error', detail: 'V07 node x is unreachable' }
    await render(summary({ blocking: [blocking], definition: { ...summary().definition, state: 'blocking' } }))
    expect(q('summary-blocking-item')!.textContent).toContain('V07 node x is unreachable')
    expect((q('summary-promote') as HTMLButtonElement).disabled).toBe(true)
  })

  it('shows a refusal as it came back and asks again with the id the server named', async () => {
    await render(summary())
    q('summary-promote')!.click()
    flushSync()
    const newer: SummaryItem = { axis: 'definition', code: 'scenario_failed', detail: 'scenario green failed at quick' }
    fetchMock.mockResolvedValueOnce(json({ code: 'needs_acknowledgement', summary_id: 'sum-2', items: [newer] }, 409))
    q('summary-confirm-promote')!.click()
    await settle()
    // The items the server holds now are what the person confirms next.
    expect(q('summary-refusal')!.textContent).toContain('changed')
    expect(q('summary-confirm')!.textContent).toContain(newer.detail)
    fetchMock.mockResolvedValueOnce(json({ verdict: 'promote' }))
    q('summary-confirm-promote')!.click()
    await settle()
    expect(lastBody()).toEqual({ expected_version: '1.0.1', expected_digest: 'sha256:cand', acknowledged: 'sum-2' })
  })

  it('reads a stale refusal as another candidate and reloads the summary', async () => {
    await render(summary({ attention: [] }))
    fetchMock.mockResolvedValueOnce(
      json({ code: 'stale', expected: { version: '1.0.1', digest: 'sha256:cand' }, found: { version: '1.0.2', digest: 'sha256:new' } }, 409),
    )
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary({ candidate: { version: '1.0.2', digest: 'sha256:new' } }) }))
    q('summary-promote')!.click()
    await settle()
    expect(q('summary-refusal')!.textContent).toContain('1.0.2')
    expect(fetchMock.mock.calls[2][0]).toBe('/api/playbooks/p/candidate/summary?workspace=w1')
    expect(q('summary-candidate')!.textContent).toContain('1.0.2')
  })

  it('runs a quick scenario of the candidate and reloads the summary', async () => {
    await render(summary())
    fetchMock.mockResolvedValueOnce(json({ report: { verdict: 'passed' } }))
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary({ attention: [fitnessItem] }) }))
    q('summary-run-quick')!.click()
    await settle()
    expect(fetchMock.mock.calls[1][0]).toBe('/api/playbooks/p/scenario-runs?workspace=w1')
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({ scenario: 'green', level: 'quick', version: '1.0.1' })
    expect(all('summary-attention-item')).toHaveLength(1)
  })

  it('keeps a failed quick run out of the promotion refusal and clears it on the next run', async () => {
    await render(summary())
    fetchMock.mockResolvedValueOnce(json({ error: 'the scenario file does not parse' }, 422))
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary() }))
    q('summary-run-quick')!.click()
    await settle()
    expect(q('summary-refusal')).toBeNull()
    expect(q('summary-run-error')!.textContent).toContain('The quick run of green failed')
    fetchMock.mockResolvedValueOnce(json({ report: { verdict: 'passed' } }))
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary() }))
    q('summary-run-quick')!.click()
    await settle()
    expect(q('summary-run-error')).toBeNull()
  })

  it('renders nothing without a candidate', async () => {
    await render(null)
    expect(q('candidate-summary')).toBeNull()
  })

  it('keeps an open confirmation through a refresh of the page', async () => {
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary() }))
    const props = $state({ id: 'p', workspace: 'w1', refresh: 0, onDecided: vi.fn() })
    app = mount(CandidateSummaryCard, { target: document.body, props })
    await settle()
    q('summary-promote')!.click()
    flushSync()
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary() }))
    props.refresh++
    await settle()
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(q('summary-confirm')).not.toBeNull()
  })

  it('promotes what the open confirmation showed even after a refresh read another candidate', async () => {
    fetchMock.mockResolvedValueOnce(json({ id: 'p', summary: summary() }))
    const props = $state({ id: 'p', workspace: 'w1', refresh: 0, onDecided: vi.fn() })
    app = mount(CandidateSummaryCard, { target: document.body, props })
    await settle()
    q('summary-promote')!.click()
    flushSync()
    fetchMock.mockResolvedValueOnce(
      json({ id: 'p', summary: summary({ summary_id: 'sum-9', candidate: { version: '1.0.2', digest: 'sha256:new' } }) }),
    )
    props.refresh++
    await settle()
    // The confirmation names what the person saw, not the summary read behind it.
    expect(q('summary-confirm')!.textContent).toContain('1.0.1')
    fetchMock.mockResolvedValueOnce(json({ verdict: 'promote' }))
    q('summary-confirm-promote')!.click()
    await settle()
    expect(lastBody()).toEqual({ expected_version: '1.0.1', expected_digest: 'sha256:cand', acknowledged: 'sum-1' })
  })
})
