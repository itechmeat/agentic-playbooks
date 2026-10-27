// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { fetchEstimate } = vi.hoisted(() => ({ fetchEstimate: vi.fn() }))
vi.mock('./api', () => ({ fetchEstimate }))

import RunStartDialog from './RunStartDialog.svelte'
import type { RunEstimate } from './api.gen'

let app: ReturnType<typeof mount> | null = null
beforeEach(() => fetchEstimate.mockReset())
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

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
    expect(onrun).toHaveBeenCalledWith({ on_no_estimate: 'pause' })
  })

  it('sends the typed budget, and no policy choice when every node is estimated', async () => {
    const onrun = await open(null, [])
    expect(byTestId('run-start-policy')).toBeNull()
    type('run-start-tokens', '300k')
    type('run-start-usd', '$2')
    submit()
    expect(onrun).toHaveBeenCalledWith({ tokens: 300_000, usd: 2 })
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
    expect(onrun).toHaveBeenCalledWith({ usd: 2, on_no_estimate: 'continue' })
  })

  it("keeps asking with the block's limit when the inputs are emptied", async () => {
    const onrun = await open({ tokens: 1000 }, ['a'])
    expect(byTestId('run-start-block-hint')).not.toBeNull()
    type('run-start-tokens', '')
    expect(byTestId('run-start-policy')).not.toBeNull()
    document.querySelector<HTMLInputElement>('input[value="pause"]')!.click()
    flushSync()
    submit()
    expect(onrun).toHaveBeenCalledWith({ on_no_estimate: 'pause' })
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

  it('starts without a budget when none is set', async () => {
    const onrun = await open(null, ['a'])
    expect(byTestId('run-start-policy')).toBeNull()
    submit()
    expect(onrun).toHaveBeenCalledWith(undefined)
  })
})
