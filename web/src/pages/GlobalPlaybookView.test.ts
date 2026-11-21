// @vitest-environment happy-dom
// GlobalPlaybookView: the read-only page of a global playbook. The api
// module is doubled; everything else renders for real.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import GlobalPlaybookView from './GlobalPlaybookView.svelte'

const api = vi.hoisted(() => ({
  fetchProjects: vi.fn(() => Promise.resolve([])),
  fetchPlaybook: vi.fn(() => Promise.resolve(detail())),
  fetchFit: vi.fn(() => Promise.resolve({})),
}))
vi.mock('../lib/api', () => api)

const toast = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

const detail = (over: Record<string, unknown> = {}) => ({
  id: 'inbox-triage',
  version: '1.0.0',
  scope: 'global',
  yaml: 'id: inbox-triage\n',
  playbook: { name: 'Inbox Triage', id: 'inbox-triage', nodes: [], edges: [] },
  validation: [],
  effective_effects: [],
  ...over,
})

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.clearAllMocks()
})

const mountPage = async () => {
  app = mount(GlobalPlaybookView, { target: document.body, props: { id: 'inbox-triage' } })
  flushSync()
  await Promise.resolve()
  flushSync()
  await Promise.resolve()
  flushSync()
}

describe('GlobalPlaybookView', () => {
  it('shows the commands and no run or edit control', async () => {
    await mountPage()
    const text = document.body.textContent ?? ''
    expect(text).toContain('apb run inbox-triage --scope global')
    expect(text).toContain('apb adopt inbox-triage --scope global')
    const links = [...document.body.querySelectorAll('a')].map((a) =>
      a.getAttribute('href'),
    )
    expect(
      links.every((h) => !h?.startsWith('#/edit/')),
      `no edit link: ${links}`,
    ).toBe(true)
    const buttons = [...document.body.querySelectorAll('button')].map((b) =>
      (b.textContent ?? '').trim(),
    )
    expect(buttons.every((t) => t !== 'Run'), `no Run button: ${buttons}`).toBe(true)
  })

  it('asks the detail with the global scope', async () => {
    await mountPage()
    expect(api.fetchPlaybook).toHaveBeenCalledWith('inbox-triage', '', undefined, 'global')
  })

  it('asks the fit of the global playbook for the picked project', async () => {
    api.fetchProjects.mockResolvedValue([
      { workspace_id: 'w1', name: 'Proj', root: '/p' },
    ] as never)
    // The answer stays pending: only the request is under test here.
    api.fetchFit.mockReturnValue(new Promise(() => {}))
    await mountPage()
    expect(api.fetchFit).toHaveBeenCalledWith('inbox-triage', 'w1', 'global')
    expect(api.fetchFit).not.toHaveBeenCalledWith('inbox-triage', 'w1')
  })

  it('without a project it shows the note and no fit', async () => {
    api.fetchProjects.mockResolvedValue([])
    await mountPage()
    const text = document.body.textContent ?? ''
    expect(text).toContain('Open a project to see whether this playbook fits it.')
    expect(api.fetchFit).not.toHaveBeenCalled()
  })
})
