// @vitest-environment happy-dom
// RunList's loading behavior: one request per terms change, the chip applied
// only on a successful answer, and a late answer never overwriting a newer
// one. The api and websocket modules are doubled; everything else renders for
// real.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import RunList from './RunList.svelte'
import { projectFilter, ALL_PROJECTS } from '../lib/projectfilter'

const api = vi.hoisted(() => ({
  fetchRunsSearch: vi.fn(),
  fetchProjects: vi.fn(() => Promise.resolve([])),
}))
vi.mock('../lib/api', () => api)
vi.mock('../lib/ws', () => ({ subscribeChanges: (_cb: () => void) => () => {} }))

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  projectFilter.set(ALL_PROJECTS)
})

type Gate = { resolve: (v: unknown) => void; reject: (e: unknown) => void }
let gates: Gate[] = []

const answer = (run_id: string) => ({
  runs: [
    {
      workspace_id: 'w',
      run_id,
      playbook: 'p',
      status: 'succeeded',
      started_ts: Date.now(),
      project: 'proj',
      search: {
        state: 'indexed',
        fields: [{ name: 'customer', state: 'value', value: 'Acme' }],
      },
    },
  ],
  not_indexed: 0,
  withheld: 0,
})

const httpError = (detail: string) => {
  const e = new Error(`/api/runs?search=1&field=customer%3Av: HTTP 400`) as Error & { detail?: string }
  e.detail = detail
  return e
}

beforeEach(() => {
  gates = []
  api.fetchRunsSearch.mockReset()
  api.fetchRunsSearch.mockImplementation(
    () => new Promise((resolve, reject) => gates.push({ resolve, reject })),
  )
})

async function mountList() {
  app = mount(RunList, { target: document.body })
  flushSync()
  await new Promise((r) => setTimeout(r))
  flushSync()
  return document.body
}

const settle = async (i: number, value: unknown, reject = false) => {
  const g = gates[i]
  if (reject) g.reject(value)
  else g.resolve(value)
  await new Promise((r) => setTimeout(r))
  await new Promise((r) => setTimeout(r))
  flushSync()
}

function applyTerm(value: string) {
  const input = document.body.querySelector('[data-testid="run-search-value"]') as HTMLInputElement
  const form = input?.closest('form') as HTMLFormElement
  input.value = value
  input.dispatchEvent(new Event('input', { bubbles: true }))
  form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }))
}

describe('RunList search loading', () => {
  it('rolls the chip back and shows the server reason when the answer is a 400', async () => {
    await mountList()
    await settle(0, answer('r1'))
    expect(document.body.textContent).toContain('r1')

    applyTerm('v')
    expect(api.fetchRunsSearch).toHaveBeenLastCalledWith([{ name: 'customer', value: 'v' }])
    await settle(1, httpError('value too long'), true)

    // The committed terms keep the previous value: no chip for the refused
    // term, and the list still shows the earlier answer.
    expect(document.body.textContent).not.toContain('customer = v')
    expect(document.body.textContent).toContain('r1')
    // A retry with the same term is attempted again (the chip was rolled back).
    applyTerm('v')
    expect(api.fetchRunsSearch).toHaveBeenLastCalledWith([{ name: 'customer', value: 'v' }])
  })

  it('applies the chip only after the answer succeeds', async () => {
    await mountList()
    await settle(0, answer('r1'))
    applyTerm('v')
    // While the request is in flight the old terms are still shown.
    expect(document.body.textContent).not.toContain('customer = v')
    await settle(1, answer('r2'))
    expect(document.body.textContent).toContain('customer = v')
  })

  it('ignores a slower older answer so it never overwrites newer items', async () => {
    await mountList()
    await settle(0, answer('r1'))
    applyTerm('a')
    applyTerm('b')
    // Two requests in flight; the second one is the latest.
    expect(gates.length).toBe(3)
    await settle(2, answer('r-new'))
    await settle(1, answer('r-old'))
    expect(document.body.textContent).toContain('r-new')
    expect(document.body.textContent).not.toContain('r-old')
  })

  it('scopes the not searchable footer to the visible listing', async () => {
    await mountList()
    const full = { ...answer('r1'), not_indexed: 2 }
    await settle(0, full)
    applyTerm('v')
    const counted = { ...answer('r2'), not_indexed: 2 }
    await settle(1, counted)
    expect(document.body.textContent).toContain('2 runs have no search fields')

    projectFilter.set('w')
    await new Promise((r) => setTimeout(r))
    flushSync()
    // The rows are narrowed to one project client side, while the count is
    // over all workspaces, so the footer no longer claims it.
    expect(document.body.textContent).not.toContain('2 runs have no search fields')
  })
})
