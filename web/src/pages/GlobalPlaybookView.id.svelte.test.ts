// @vitest-environment happy-dom
// GlobalPlaybookView reused across routes: a new id starts from a clean
// state, so an earlier failure or detail never shows for it.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import GlobalPlaybookView from './GlobalPlaybookView.svelte'

const api = vi.hoisted(() => ({
  fetchProjects: vi.fn(() => Promise.resolve([])),
  fetchPlaybook: vi.fn(),
  fetchFit: vi.fn(() => Promise.resolve({})),
}))
vi.mock('../lib/api', () => api)
const toast = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

const detail = (id: string, name: string) => ({
  id,
  version: '1.0.0',
  scope: 'global',
  yaml: `id: ${id}\n`,
  playbook: { name, id, nodes: [], edges: [] },
  validation: [],
  effective_effects: [],
})

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.clearAllMocks()
})

const settle = async () => {
  for (let i = 0; i < 10; i++) {
    await Promise.resolve()
    flushSync()
  }
}

describe('GlobalPlaybookView on a new id', () => {
  it('drops the earlier failure and detail', async () => {
    const props = $state({ id: 'first' })
    api.fetchPlaybook.mockRejectedValueOnce(new Error('gone'))
    app = mount(GlobalPlaybookView, { target: document.body, props })
    await settle()
    const gone = 'The global playbook could not be loaded.'
    expect(document.body.textContent ?? '').toContain(gone)

    let release: (v: unknown) => void = () => {}
    api.fetchPlaybook.mockReturnValueOnce(new Promise((r) => (release = r)))
    props.id = 'second'
    await settle()
    expect(document.body.textContent ?? '').not.toContain(gone)

    release(detail('second', 'Second Playbook'))
    await settle()
    expect(document.body.textContent ?? '').toContain('Second Playbook')
  })

  it('does not show the previous detail while the next one loads', async () => {
    const props = $state({ id: 'first' })
    api.fetchPlaybook.mockResolvedValueOnce(detail('first', 'First Playbook'))
    app = mount(GlobalPlaybookView, { target: document.body, props })
    await settle()
    expect(document.body.textContent ?? '').toContain('First Playbook')

    api.fetchPlaybook.mockReturnValueOnce(new Promise(() => {}))
    props.id = 'second'
    await settle()
    expect(document.body.textContent ?? '').not.toContain('First Playbook')
  })
})
