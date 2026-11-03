// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import JudgeSandboxPanel from './JudgeSandboxPanel.svelte'
import type { RunValues, SandboxEvaluation } from './api.gen'
import { auth } from './auth.svelte'

// The panel evaluates over HTTP (`POST .../judge/{node}/evaluate`, `GET
// /api/runs/{run}/judge/{node}/values`): the test stubs `fetch` and checks
// what the panel sends and how it shows the answer. Runes (a page that
// hands the panel a new node object) need the `.svelte.` infix.
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

const node = {
  id: 'check',
  state: { task: '{{run.instruction}}', clarification: '{{nodes.clarify.review_note}}' },
  questions: {
    sufficient: { type: 'choice', instructions: 'Clear?', criteria: { clear: 'a', unclear: 'b' } },
    risky: { type: 'noul', instructions: 'Risky?' },
  },
}

const evaluation: SandboxEvaluation = {
  playbook: 'p',
  version: '1.0.0',
  node: 'check',
  evaluation: {
    node_digest: 'j1:sha256:n',
    state_digest: 'sha256:s',
    questions_digest: 'sha256:q',
    fields: [
      { name: 'task', class: 'prompt', bytes: 14, clipped: false },
      { name: 'clarification', class: 'prompt', bytes: 0, clipped: false, unfilled: ['nodes.clarify.review_note'] },
    ],
    raw: { sufficient: { value: 'clear', confidence: 0.62 }, risky: { p: 0.1 } },
    mode: 'shadow',
    at_enforce: { kind: 'output', output: { sufficient: 'unclear', risky: false } },
    as_configured: { kind: 'fallback', reason: 'shadow keeps the answers from applying', output: { decided_by: 'unavailable' } },
    would_change: true,
    requests: 0,
    cost_usd: null,
  },
  route_at_enforce: ['e:check>clarify#0'],
  route_as_configured: ['e:check>clarify#1'],
  route_note: 'The route preview reads only this node',
  requests_today: 4,
  daily_cap: 100,
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

function type(el: HTMLElement | null, value: string, event = 'input') {
  const input = el as HTMLInputElement
  input.value = value
  input.dispatchEvent(new Event(event, { bubbles: true }))
  flushSync()
}

// The text of each input's label, in order.
const slotLabels = () =>
  [...document.querySelectorAll('[data-testid^="sandbox-slot-"] > .mono')].map((e) => e.textContent)

function render() {
  app = mount(JudgeSandboxPanel, { target: document.body, props: { id: 'p', workspace: 'w1', node } })
  flushSync()
}

const sent = () => JSON.parse(String(fetchMock.mock.lastCall?.[1]?.body))

describe('JudgeSandboxPanel', () => {
  it('evaluates the typed inputs with the given answers and shows the three outputs and the route side by side', async () => {
    render()
    expect([...document.querySelectorAll<HTMLElement>('textarea[data-key]')].map((i) => i.dataset.key)).toEqual(['instruction', 'reviews.clarify.note'])
    type(q('sandbox-input-instruction'), 'Fix the parser')
    // An input left empty renders as nothing, and the panel says so.
    expect(q('sandbox-slot-reviews.clarify.note')!.textContent).toContain('renders as nothing')
    type(q('sandbox-answer-sufficient'), 'clear', 'change')
    type(q('sandbox-confidence-sufficient'), '0.62')
    type(q('sandbox-p-risky'), '0.1')

    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(fetchMock.mock.lastCall?.[0]).toBe('/api/playbooks/p/judge/check/evaluate?workspace=w1')
    expect(sent()).toEqual({
      inputs: {
        params: {},
        instruction: 'Fix the parser',
        outputs: {},
        reviews: { clarify: { decision: '', note: '' } },
        rejected_outputs: {},
        hooks: {},
        context: '',
      },
      answers: { sufficient: { value: 'clear', confidence: 0.62 }, risky: { p: 0.1 } },
      emulate: false,
    })
    expect(q('sandbox-raw')!.textContent).toContain('clear')
    expect(q('sandbox-raw')!.textContent).toContain('0.62')
    expect(q('sandbox-at-enforce')!.textContent).toContain('unclear')
    expect(q('sandbox-as-configured')!.textContent).toContain('On this machine')
    expect(q('sandbox-as-configured')!.textContent).toContain('shadow keeps the answers from applying')
    expect(q('sandbox-route-at-enforce')!.textContent).toContain('e:check>clarify#0')
    expect(q('sandbox-route-as-configured')!.textContent).toContain('e:check>clarify#1')
    expect(q('sandbox-route-note')!.textContent).toContain('reads only this node')
    expect(q('sandbox-unfilled')!.textContent).toContain('nodes.clarify.review_note')
    expect(q('sandbox-spend')!.textContent).toContain('4 of 100')
  })

  it('asks the machine without answers, emulating when asked, and shows a refusal as the server gave it', async () => {
    render()
    const ask = q('sandbox-source-ask') as HTMLInputElement
    ask.checked = true
    ask.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    expect(q('sandbox-answer-sufficient')).toBeNull()
    const emulate = q('sandbox-emulate') as HTMLInputElement
    emulate.checked = true
    emulate.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    fetchMock.mockResolvedValueOnce(json({ error: 'a live ask is refused by a dashboard without keys' }, 409))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent()).not.toHaveProperty('answers')
    expect(sent().emulate).toBe(true)
    // The server's sentence, without the request's address.
    expect(q('sandbox-error')!.textContent).toBe('a live ask is refused by a dashboard without keys')
  })

  it('offers the live ask only with keys: a dashboard without keys points to the command line', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    render()
    expect((q('sandbox-source-ask') as HTMLInputElement).disabled).toBe(true)
    expect(q('sandbox-ask-keyless')!.textContent).toContain('apb judge eval')
    auth.set({ required: true, authenticated: true, checked: true })
    flushSync()
    expect((q('sandbox-source-ask') as HTMLInputElement).disabled).toBe(false)
    expect(q('sandbox-ask-keyless')).toBeNull()
  })

  it('names a place target as the input it fills is labelled', async () => {
    render()
    const labels = slotLabels()
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 1,
      inputs: { params: {}, instruction: 'x', outputs: {}, reviews: {}, rejected_outputs: {}, hooks: {}, context: '' },
      recorded: [{ label: 'outputs.check', text: 'done' }],
    }
    type(q('sandbox-run-id'), 'r1')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    const options = [...(q('sandbox-place-target-0') as HTMLSelectElement).options].map((o) => o.textContent)
    expect(options).toEqual(labels)
    expect(labels).toEqual(['run.instruction', 'nodes.clarify.review_note'])
  })

  it('shows the numbers a run recorded rounded, never as float noise', async () => {
    render()
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 1,
      inputs: { params: {}, instruction: 'x', outputs: {}, reviews: {}, rejected_outputs: {}, hooks: {}, context: '' },
      recorded: [],
      observed: {
        provider: 'fake',
        model: 'fake-1',
        answers: { sufficient: { value: 'unclear', confidence: 0.8999999761581421 }, risky: { p: 0.30000001192092896 } },
      },
    }
    type(q('sandbox-run-id'), 'r1')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    expect((q('sandbox-confidence-sufficient') as HTMLInputElement).value).toBe('0.9')
    expect((q('sandbox-p-risky') as HTMLInputElement).value).toBe('0.3')
    const noisy = {
      ...evaluation,
      evaluation: {
        ...evaluation.evaluation,
        at_enforce: { kind: 'output' as const, output: { sufficient: 'unclear', sufficient_p: 0.8999999761581421 } },
      },
    }
    fetchMock.mockResolvedValueOnce(json(noisy))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(q('sandbox-result')!.textContent).toContain('sufficient_p 0.9')
    expect(q('sandbox-result')!.textContent).not.toContain('0.89999')
  })

  it('evaluates unchanged recorded answers with their full values and typed ones as typed', async () => {
    const scored = {
      id: 'check',
      state: { task: '{{run.instruction}}' },
      questions: {
        effort: { type: 'score', instructions: 'How much?', levels: ['small', 'medium', 'large'] },
        risky: { type: 'noul', instructions: 'Risky?' },
      },
    }
    app = mount(JudgeSandboxPanel, { target: document.body, props: { id: 'p', workspace: 'w1', node: scored } })
    flushSync()
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 1,
      inputs: { params: {}, instruction: 'x', outputs: {}, reviews: {}, rejected_outputs: {}, hooks: {}, context: '' },
      recorded: [],
      observed: {
        provider: 'fake',
        model: 'fake-1',
        answers: { effort: { value: 1.234567, confidence: 0.8999999761581421 }, risky: { p: 0.30000001192092896 } },
      },
    }
    type(q('sandbox-run-id'), 'r1')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    // Shown rounded, evaluated in full: a band boundary at 1.234568 routes
    // as the run did.
    expect((q('sandbox-confidence-effort') as HTMLInputElement).value).toBe('0.9')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent().answers).toEqual({
      effort: { value: 1.234567, confidence: 0.8999999761581421 },
      risky: { p: 0.30000001192092896 },
    })
    // A typed change is evaluated as typed.
    type(q('sandbox-confidence-effort'), '0.5')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent().answers.effort).toEqual({ value: 1.234567, confidence: 0.5 })
    // Typed back to the shown text, it is what the person typed now, not
    // the recorded value: a threshold between them routes as typed.
    type(q('sandbox-p-risky'), '0.2')
    type(q('sandbox-p-risky'), '0.3')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent().answers.risky).toEqual({ p: 0.3 })
  })

  it('fills the inputs from a judge execution of a run and places a recorded value', async () => {
    render()
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 2,
      inputs: {
        params: {},
        instruction: 'Fix the parser',
        outputs: {},
        reviews: { clarify: { decision: 'clarified', note: 'only YAML' } },
        rejected_outputs: {},
        hooks: {},
        context: '',
      },
      recorded: [{ label: 'outputs.work', text: 'patched parse.rs' }],
      observed: { provider: 'fake', model: 'fake-1', answers: { sufficient: { value: 'unclear', confidence: 0.4 } } },
    }
    type(q('sandbox-run-id'), 'r1')
    type(q('sandbox-execution'), '2')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    expect(fetchMock.mock.lastCall?.[0]).toBe('/api/runs/r1/judge/check/values?workspace=w1&execution=2')
    expect((q('sandbox-input-instruction') as HTMLTextAreaElement).value).toBe('Fix the parser')
    expect((q('sandbox-input-reviews.clarify.note') as HTMLTextAreaElement).value).toBe('only YAML')
    // The run's own answers become the given ones, so a free evaluation shows
    // what today's thresholds make of them.
    expect((q('sandbox-answer-sufficient') as HTMLSelectElement).value).toBe('unclear')
    type(q('sandbox-place-target-0'), 'instruction', 'change')
    q('sandbox-place-0')!.click()
    flushSync()
    expect((q('sandbox-input-instruction') as HTMLTextAreaElement).value).toBe('patched parse.rs')
  })

  it('keeps the inputs and the answer when the page hands it the same node again', async () => {
    const props = $state({ id: 'p', workspace: 'w1', node: { ...node } })
    app = mount(JudgeSandboxPanel, { target: document.body, props })
    flushSync()
    type(q('sandbox-input-instruction'), 'Fix the parser')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    // A reload of the playbook page: the same node, a new object.
    props.node = { ...node }
    flushSync()
    expect(q('sandbox-result')).not.toBeNull()
    expect((q('sandbox-input-instruction') as HTMLTextAreaElement).value).toBe('Fix the parser')
    // Another node starts afresh.
    props.node = { ...node, id: 'other' }
    flushSync()
    expect(q('sandbox-result')).toBeNull()
  })

  it('starts another node with given answers, not a live ask', async () => {
    const props = $state({ id: 'p', workspace: 'w1', node: { ...node } })
    app = mount(JudgeSandboxPanel, { target: document.body, props })
    flushSync()
    const ask = q('sandbox-source-ask') as HTMLInputElement
    ask.checked = true
    ask.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    const emulate = q('sandbox-emulate') as HTMLInputElement
    emulate.checked = true
    emulate.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    props.node = { ...node, id: 'other' }
    flushSync()
    expect((q('sandbox-source-given') as HTMLInputElement).checked).toBe(true)
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent()).toHaveProperty('answers')
    expect(sent().emulate).toBe(false)
  })

  it('drops the earlier answers when an execution without observed answers fills the inputs', async () => {
    render()
    type(q('sandbox-answer-sufficient'), 'clear', 'change')
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 1,
      inputs: { params: {}, instruction: 'Fix it', outputs: {}, reviews: {}, rejected_outputs: {}, hooks: {}, context: '' },
      recorded: [],
    }
    type(q('sandbox-run-id'), 'r1')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    expect((q('sandbox-answer-sufficient') as HTMLSelectElement).value).toBe('')
  })

  it('gives a score as the number of its level, as the engine reads it, and keeps an observed expected level', async () => {
    const scored = {
      id: 'check',
      state: { task: '{{run.instruction}}' },
      questions: { effort: { type: 'score', instructions: 'How much?', levels: ['small', 'medium', 'large'] } },
    }
    app = mount(JudgeSandboxPanel, { target: document.body, props: { id: 'p', workspace: 'w1', node: scored } })
    flushSync()
    const select = q('sandbox-answer-effort') as HTMLSelectElement
    // The levels by name, lowest first; the value the engine takes is the index.
    expect([...select.options].map((o) => o.textContent)).toEqual(['no answer', 'small', 'medium', 'large'])
    type(select, '1', 'change')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent().answers).toEqual({ effort: { value: 1 } })

    // A run's observed answer is the expected level, which may lie between two.
    const values: RunValues = {
      run_id: 'r1',
      node: 'check',
      execution: 1,
      inputs: { params: {}, instruction: 'x', outputs: {}, reviews: {}, rejected_outputs: {}, hooks: {}, context: '' },
      recorded: [],
      observed: { provider: 'fake', model: 'fake-1', answers: { effort: { value: 1.4, confidence: 0.7 } } },
    }
    type(q('sandbox-run-id'), 'r1')
    fetchMock.mockResolvedValueOnce(json(values))
    q('sandbox-fill')!.click()
    await settle()
    expect((q('sandbox-answer-effort') as HTMLSelectElement).value).toBe('1.4')
    fetchMock.mockResolvedValueOnce(json(evaluation))
    q('sandbox-evaluate')!.click()
    await settle()
    expect(sent().answers).toEqual({ effort: { value: 1.4, confidence: 0.7 } })
  })
})
