// @vitest-environment happy-dom
// TemplateList: the template gallery page. The api module is doubled;
// everything else renders for real.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import TemplateList from './TemplateList.svelte'

const template = {
  id: 'inbox-triage',
  name: 'Inbox triage',
  description: 'Triage an inbox.',
  version: '1.0.0',
  effects: [],
  scenarios: 0,
  digest: 'sha256:aa',
}

const answer = (scope: string) => ({
  id: 'inbox-triage',
  version: '1.0.0',
  scope,
  template: 'inbox-triage',
  lifecycle: 'draft',
  scenarios: 0,
  fit: null,
  needs: [],
  warnings: [],
})

const api = vi.hoisted(() => ({
  fetchTemplates: vi.fn(() => Promise.resolve([] as unknown[])),
  fetchProjects: vi.fn(() => Promise.resolve([] as unknown[])),
  installTemplate: vi.fn((_id: string, body: { scope: string }) =>
    Promise.resolve(answer(body.scope)),
  ),
}))
vi.mock('../lib/api', () => api)
const toast = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

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

const mountPage = async (projects: unknown[]) => {
  api.fetchTemplates.mockResolvedValue([template])
  api.fetchProjects.mockResolvedValue(projects)
  app = mount(TemplateList, { target: document.body })
  await settle()
}

const selects = () => {
  const [project, scope] = [...document.body.querySelectorAll('select')]
  return { project, scope }
}

const pick = async (select: HTMLSelectElement, value: string) => {
  select.value = value
  select.dispatchEvent(new Event('change', { bubbles: true }))
  await settle()
}

const install = async () => {
  const button = [...document.body.querySelectorAll('button')].find((b) =>
    (b.textContent ?? '').startsWith('Install'),
  )!
  button.click()
  await settle()
}

const projects = [
  { workspace_id: 'w1', name: 'One', root: '/one' },
  { workspace_id: 'w2', name: 'Two', root: '/two' },
]

describe('TemplateList', () => {
  it('a global install names the selected project for its fit report', async () => {
    await mountPage(projects)
    const { project, scope } = selects()
    await pick(scope, 'global')
    expect(project.disabled, 'the project select stays enabled').toBe(false)
    await pick(project, 'w2')
    await install()
    expect(api.installTemplate).toHaveBeenCalledWith('inbox-triage', {
      scope: 'global',
      workspace: 'w2',
    })
  })

  it('without a reachable project the scope starts at global', async () => {
    await mountPage([])
    expect(selects().scope.value).toBe('global')
    await install()
    expect(api.installTemplate).toHaveBeenCalledWith('inbox-triage', { scope: 'global' })
  })

  it('when the project list fails the scope is global and no workspace is sent', async () => {
    api.fetchTemplates.mockResolvedValue([template])
    api.fetchProjects.mockRejectedValue(new Error('down'))
    app = mount(TemplateList, { target: document.body })
    await settle()
    expect(selects().scope.value).toBe('global')
    await install()
    expect(api.installTemplate).toHaveBeenCalledWith('inbox-triage', { scope: 'global' })
  })

  it('the Open link keeps the project the install used', async () => {
    await mountPage(projects)
    await install()
    expect(api.installTemplate).toHaveBeenCalledWith('inbox-triage', {
      scope: 'project',
      workspace: 'w1',
    })
    await pick(selects().project, 'w2')
    const link = [...document.body.querySelectorAll('a')].find((a) =>
      (a.textContent ?? '').includes('Open inbox-triage'),
    )!
    expect(link.getAttribute('href')).toBe('#/playbook/w1/inbox-triage')
  })

  it('a project install waits for the project list', async () => {
    api.fetchTemplates.mockResolvedValue([template])
    api.fetchProjects.mockReturnValue(new Promise(() => {}))
    app = mount(TemplateList, { target: document.body })
    await settle()
    const button = () =>
      [...document.body.querySelectorAll('button')].find((b) =>
        (b.textContent ?? '').startsWith('Install'),
      )!
    expect(button().disabled, 'no project install before the list').toBe(true)
    await pick(selects().scope, 'global')
    expect(button().disabled, 'a global install needs no project').toBe(false)
  })

  it('each pending install keeps its own button disabled', async () => {
    const other = { ...template, id: 'other', name: 'Other' }
    api.fetchTemplates.mockResolvedValue([template, other])
    api.fetchProjects.mockResolvedValue(projects)
    const done: Record<string, (v: unknown) => void> = {}
    api.installTemplate.mockImplementation(
      (id: string, body: { scope: string }) =>
        new Promise((r) => (done[id] = () => r({ ...answer(body.scope), id }))) as never,
    )
    app = mount(TemplateList, { target: document.body })
    await settle()
    const buttons = () =>
      [...document.body.querySelectorAll('button')].filter((b) =>
        (b.textContent ?? '').startsWith('Install'),
      )
    buttons()[0].click()
    await settle()
    // The second card's button is the only Install one left enabled.
    const second = [...document.body.querySelectorAll('section.card')][1].querySelector('button')!
    second.click()
    await settle()
    const cards = () =>
      [...document.body.querySelectorAll('section.card')].map(
        (c) => c.querySelector('button')!.disabled,
      )
    expect(cards()).toEqual([true, true])
    done['inbox-triage'](null)
    await settle()
    expect(cards(), 'only the finished install is enabled again').toEqual([false, true])
    second.click()
    await settle()
    expect(api.installTemplate).toHaveBeenCalledTimes(2)
  })
})
