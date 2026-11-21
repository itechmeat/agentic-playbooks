// @vitest-environment happy-dom
// PlaybookList's global rows: a global playbook links to its global page
// and carries no Duplicate and no Delete. The api module is doubled.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import PlaybookList from './PlaybookList.svelte'
import { projectFilter, ALL_PROJECTS } from '../lib/projectfilter'

const api = vi.hoisted(() => ({
  deletePlaybook: vi.fn(),
  fetchPlaybook: vi.fn(),
  fetchPlaybooks: vi.fn(() =>
    Promise.resolve([
      {
        id: 'mine',
        name: 'Mine',
        current: '1.0.0',
        versions: ['1.0.0'],
        workspace_id: 'w1',
        project: 'Proj',
        scope: 'project',
        node_count: 2,
        frozen: false,
      },
      {
        id: 'shared',
        name: 'Shared',
        current: '1.0.0',
        versions: ['1.0.0'],
        workspace_id: '',
        project: '',
        scope: 'global',
        node_count: 2,
        frozen: false,
      },
    ]),
  ),
  fetchRuns: vi.fn(() => Promise.resolve([])),
  fetchStats: vi.fn(() => Promise.resolve({})),
  installDemo: vi.fn(),
}))
vi.mock('../lib/api', () => api)
vi.mock('../lib/ws', () => ({
  subscribeChanges: (_cb: () => void) => () => {},
  slowRefresh: (_cb: () => void, _ms?: number) => ({ request: () => {}, dispose: () => {} }),
}))
const toast = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  projectFilter.set(ALL_PROJECTS)
  vi.clearAllMocks()
})

const settle = async () => {
  for (let i = 0; i < 10; i++) {
    await Promise.resolve()
    flushSync()
  }
}

describe('PlaybookList', () => {
  it('counts projects without the global group and keeps the project label', async () => {
    app = mount(PlaybookList, { target: document.body })
    await settle()
    const text = document.body.textContent ?? ''
    expect(text).toContain('2 playbooks')
    expect(text, 'the global group is no project').not.toContain('2 projects')
    expect(text).toContain('project · Proj')
  })

  it('keeps the project label with a project selected', async () => {
    projectFilter.set('w1')
    app = mount(PlaybookList, { target: document.body })
    await settle()
    const text = document.body.textContent ?? ''
    expect(text).toContain('project · Proj')
    expect(text).not.toContain('project · Global playbooks')
  })

  it('the picker offers the projects and no entry for the global group', async () => {
    app = mount(PlaybookList, { target: document.body })
    await settle()
    const trigger = document.body.querySelector('[aria-label="Project filter"]') as HTMLElement
    expect(trigger, 'the project picker').toBeTruthy()
    trigger.dispatchEvent(
      new PointerEvent('pointerdown', { bubbles: true, button: 0, pointerType: 'mouse' }),
    )
    await vi.waitFor(() => expect(document.querySelector('[role="option"]')).not.toBeNull())
    const options = [...document.body.querySelectorAll('[role="option"]')].map((o) =>
      (o.textContent ?? '').trim(),
    )
    expect(options).toEqual(['All projects', 'Proj'])
  })

  it('a global row has no Duplicate and no Delete and links to the global page', async () => {
    app = mount(PlaybookList, { target: document.body })
    for (let i = 0; i < 10; i++) {
      await Promise.resolve()
      flushSync()
    }
    const row = [...document.body.querySelectorAll('a')].find((a) =>
      (a.getAttribute('href') ?? '').includes('global-playbook/shared'),
    )
    expect(row, 'the global row links to #/global-playbook/shared').toBeTruthy()
    // No Duplicate and no Delete on the global row (they read a project);
    // the project row keeps its own.
    const li = row!.closest('li')
    const rowButtons = [...(li?.querySelectorAll('button') ?? [])].map((b) =>
      b.getAttribute('aria-label'),
    )
    expect(rowButtons, 'no Duplicate and no Delete on the global row').toEqual([])
  })
})
