// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { fetchEstimate, fetchMemory, fetchNotify } = vi.hoisted(() => ({
  fetchEstimate: vi.fn(),
  fetchMemory: vi.fn(),
  fetchNotify: vi.fn(),
}))
vi.mock('./api', () => ({ fetchEstimate, fetchMemory, fetchNotify }))

import RunStartDialog from './RunStartDialog.svelte'
import type { RunEstimate } from './api.gen'

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  fetchEstimate.mockReset()
  fetchMemory.mockReset()
  fetchMemory.mockResolvedValue(emptyRecall)
  fetchNotify.mockReset()
  fetchNotify.mockResolvedValue({ on: [], default_on: false })
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const emptyRecall = {
  id: 'p',
  scope: 'project',
  version: '1.0.0',
  instruction: null,
  instruction_source: 'none',
  values: [],
  missing: [],
  schema_changed: false,
  diagnostics: [],
}

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const tick = () => new Promise((r) => setTimeout(r, 0))

const estimate = (unestimated: string[], noCost: string[] = unestimated): RunEstimate & { summary: string } => ({
  playbook: 'p',
  version: '1.0.0',
  execution: 'cli',
  comparable: {
    runs: 2,
    succeeded: 2,
    window: 50,
    runs_without_usage: 0,
    attempts_without_usage: 0,
    attempts_without_cost: 0,
  },
  time: { basis: 'none' },
  tokens: { basis: 'history', spread: { n: 2, p50: 400, low: 300, high: 500, kind: 'min_max' } },
  cost_usd: null,
  nodes: ['a', 'b'].map((node) => ({ node, visits_p50: 1, per_visit: {}, per_run: {}, per_attempt_p50: {} })),
  unestimated,
  unestimated_tokens: unestimated,
  unestimated_cost: noCost,
  summary: '',
})

async function open(
  block: unknown,
  unestimated: string[] | Error,
  extra: Record<string, unknown> = {},
  noCost?: string[],
) {
  if (unestimated instanceof Error) fetchEstimate.mockRejectedValueOnce(unestimated)
  else fetchEstimate.mockResolvedValue(estimate(unestimated, noCost ?? unestimated))
  const onrun = vi.fn()
  app = mount(RunStartDialog, {
    target: document.body,
    props: { open: true, playbookId: 'p', workspace: 'w', block, onrun, ...extra } as never,
  })
  flushSync()
  await tick()
  flushSync()
  return onrun
}

function type(id: string, value: string) {
  const input = byTestId(id) as HTMLInputElement
  input.value = value
  input.dispatchEvent(new Event('input', { bubbles: true }))
  flushSync()
}

function submit() {
  byTestId('run-start-submit')?.closest('form')?.dispatchEvent(
    new Event('submit', { bubbles: true, cancelable: true }),
  )
  flushSync()
}

describe('RunStartDialog', () => {
  it('shows the estimate and asks pause or continue only for unestimated nodes under ask', async () => {
    const onrun = await open({ tokens: 1000 }, ['a'])
    expect(fetchEstimate).toHaveBeenCalledWith('p', 'w')
    expect(byTestId('run-start-estimate')?.textContent).toContain('~400 (300-500)')
    expect(byTestId('run-start-unestimated')?.textContent).toContain('a (no history)')
    expect((byTestId('run-start-tokens') as HTMLInputElement).value).toBe('1000')
    expect(byTestId('run-start-policy')).not.toBeNull()
    submit()
    expect(onrun).not.toHaveBeenCalled()
    expect(byTestId('run-start-error')?.textContent).toContain('pauses or continues')
    const pause = document.querySelector<HTMLInputElement>('input[value="pause"]')
    pause!.click()
    flushSync()
    submit()
    expect(onrun).toHaveBeenCalledWith({ on_no_estimate: 'pause' }, { params: {} })
  })

  it('sends the typed budget, and no policy choice when every node is estimated', async () => {
    const onrun = await open(null, [])
    expect(byTestId('run-start-policy')).toBeNull()
    type('run-start-tokens', '300k')
    type('run-start-usd', '$2')
    submit()
    expect(onrun).toHaveBeenCalledWith({ tokens: 300_000, usd: 2 }, { params: {} })
  })

  it('asks for a dollar limit when the nodes have token history only', async () => {
    const onrun = await open(null, [], {}, ['a', 'b'])
    expect(byTestId('run-start-unestimated')).toBeNull()
    type('run-start-usd', '2')
    expect(byTestId('run-start-policy')).not.toBeNull()
    expect(byTestId('run-start-unestimated')?.textContent).toBe('Not estimated: a (no cost figure), b (no cost figure)')
    document.querySelector<HTMLInputElement>('input[value="continue"]')!.click()
    flushSync()
    submit()
    expect(onrun).toHaveBeenCalledWith({ usd: 2, on_no_estimate: 'continue' }, { params: {} })
  })

  it("keeps asking with the block's limit when the inputs are emptied", async () => {
    const onrun = await open({ tokens: 1000 }, ['a'])
    expect(byTestId('run-start-block-hint')).not.toBeNull()
    type('run-start-tokens', '')
    expect(byTestId('run-start-policy')).not.toBeNull()
    document.querySelector<HTMLInputElement>('input[value="pause"]')!.click()
    flushSync()
    submit()
    expect(onrun).toHaveBeenCalledWith({ on_no_estimate: 'pause' }, { params: {} })
  })

  it('asks when the estimate cannot be read', async () => {
    await open({ tokens: 1000 }, new Error('history unreadable'))
    expect(byTestId('run-start-estimate')?.textContent).toContain('history unreadable')
    expect(byTestId('run-start-policy')).not.toBeNull()
  })

  it('a retry after a refusal shows the choice even when the estimate has none', async () => {
    await open({ tokens: 1000 }, [], { retry: true })
    expect(byTestId('run-start-policy')).not.toBeNull()
  })

  it('offers Notify me only with a machine target and sends the choice when it differs', async () => {
    const onrun = await open(null, [])
    expect(byTestId('run-start-notify')).toBeNull()
    submit()
    expect(onrun).toHaveBeenLastCalledWith(undefined, { params: {} })
    unmount(app!)
    app = null

    const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }
    fetchNotify.mockResolvedValue({ target, on: ['review'], default_on: false })
    const optIn = await open(null, [])
    const box = byTestId('run-start-notify-me') as HTMLInputElement
    expect(byTestId('run-start-notify')?.textContent).toContain('telegram/me send_message to 42')
    expect(box.checked).toBe(false)
    box.click()
    flushSync()
    submit()
    expect(optIn).toHaveBeenLastCalledWith(undefined, { params: {}, notify: true })
    unmount(app!)
    app = null

    fetchNotify.mockResolvedValue({ target, on: ['review'], default_on: true })
    const byDefault = await open(null, [])
    expect((byTestId('run-start-notify-me') as HTMLInputElement).checked).toBe(true)
    // The hint names what this machine sends, not every transition.
    expect(byTestId('run-start-notify')?.textContent).toContain('About a review waiting, through')
    expect(byTestId('run-start-notify')?.textContent).not.toContain('the end of the run')
    submit()
    expect(byDefault).toHaveBeenLastCalledWith(undefined, { params: {} })
    unmount(app!)
    app = null

    // Unticking a default is the one way to turn it off for this run.
    const optOut = await open(null, [])
    ;(byTestId('run-start-notify-me') as HTMLInputElement).click()
    flushSync()
    submit()
    expect(optOut).toHaveBeenLastCalledWith(undefined, { params: {}, notify: false })
  })

  it('says when the notification settings cannot be read', async () => {
    fetchNotify.mockRejectedValue(new Error('offline'))
    await open(null, [])
    expect(byTestId('run-start-notify')).toBeNull()
    expect(byTestId('run-start-notify-note')?.textContent).toBe('The notification settings could not be read.')
    unmount(app!)
    app = null

    const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }
    fetchNotify.mockResolvedValue({ target, on: ['review'], default_on: false, problem: 'invalid project config' })
    await open(null, [])
    expect(byTestId('run-start-notify-note')?.textContent).toBe('invalid project config')
  })

  it('starts without a budget when none is set', async () => {
    const onrun = await open(null, ['a'])
    expect(byTestId('run-start-policy')).toBeNull()
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, { params: {} })
  })

  it('prefills the remembered input and sends it with the remember choice', async () => {
    fetchMemory.mockResolvedValue({
      ...emptyRecall,
      instruction: 'since the last tag',
      instruction_source: 'memory',
      values: [
        { name: 'channel', value: 'beta', status: { status: 'compatible' } },
        { name: 'count', value: 'many', status: { status: 'incompatible', reason: 'type_changed' } },
      ],
    })
    const onrun = await open(null, [], {
      params: [
        { name: 'channel', type: 'text' },
        { name: 'count', type: 'int', default: 2 },
        { name: 'token', type: 'text', secret: true, default: undefined },
      ],
    })
    expect(fetchMemory).toHaveBeenCalledWith('p', 'w')
    expect((byTestId('run-start-param-channel') as HTMLInputElement).value).toBe('beta')
    // The value that no longer fits is not used: the field shows the default.
    expect((byTestId('run-start-param-count') as HTMLInputElement).value).toBe('2')
    expect(byTestId('run-start-param-note-count')?.textContent).toContain('right type')
    expect((byTestId('run-start-param-token') as HTMLInputElement).type).toBe('password')
    expect((byTestId('run-start-instruction') as HTMLTextAreaElement).value).toBe('since the last tag')
    // The secret has no value yet: the start is held until it is entered.
    submit()
    expect(onrun).not.toHaveBeenCalled()
    expect(byTestId('run-start-error')?.textContent).toContain('token')
    type('run-start-param-token', 's3cret')
    ;(byTestId('run-start-remember') as HTMLInputElement).click()
    flushSync()
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, {
      params: { channel: 'beta', token: 's3cret' },
      instruction: 'since the last tag',
      remember: true,
    })
  })

  it('keeps what the person typed before the memory answer and waits for it to start', async () => {
    let answer: (r: unknown) => void = () => {}
    fetchMemory.mockReturnValue(new Promise((r) => (answer = r)))
    const onrun = await open(null, [], {
      params: [
        { name: 'channel', type: 'text' },
        { name: 'mode', type: 'text' },
      ],
    })
    type('run-start-param-channel', 'typed')
    type('run-start-instruction', 'my own words')
    expect((byTestId('run-start-submit') as HTMLButtonElement).disabled).toBe(true)
    submit()
    expect(onrun).not.toHaveBeenCalled()

    answer({
      ...emptyRecall,
      instruction: 'remembered words',
      instruction_source: 'memory',
      values: [
        { name: 'channel', value: 'beta', status: { status: 'compatible' } },
        { name: 'mode', value: 'fast', status: { status: 'compatible' } },
      ],
      diagnostics: ['the remembered input could not be read; it is ignored'],
    })
    await tick()
    flushSync()
    expect((byTestId('run-start-param-channel') as HTMLInputElement).value).toBe('typed')
    expect((byTestId('run-start-param-mode') as HTMLInputElement).value).toBe('fast')
    expect((byTestId('run-start-instruction') as HTMLTextAreaElement).value).toBe('my own words')
    expect(byTestId('run-start-memory-note')?.textContent).toContain('could not be read')
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, {
      params: { channel: 'typed', mode: 'fast' },
      instruction: 'my own words',
    })
  })

  it('a cleared remembered value starts with the default, not the remembered one', async () => {
    fetchMemory.mockResolvedValue({
      ...emptyRecall,
      values: [{ name: 'mode', value: 'fast', status: { status: 'compatible' } }],
    })
    const onrun = await open(null, [], {
      params: [{ name: 'mode', type: 'text', default: 'slow' }],
    })
    expect((byTestId('run-start-param-mode') as HTMLInputElement).value).toBe('fast')
    type('run-start-param-mode', '')
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, { params: { mode: 'slow' } })
  })

  it('sends the shown defaults when the remembered input cannot be read', async () => {
    fetchMemory.mockRejectedValue(new Error('gone'))
    const onrun = await open(null, [], {
      params: [{ name: 'mode', type: 'text', default: 'slow' }],
    })
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, { params: { mode: 'slow' } })
  })

  it('still starts when the remembered input cannot be read', async () => {
    fetchMemory.mockRejectedValue(new Error('gone'))
    const onrun = await open(null, [])
    expect(byTestId('run-start-memory-note')?.textContent).toContain('could not be read')
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined, { params: {} })
  })
})
