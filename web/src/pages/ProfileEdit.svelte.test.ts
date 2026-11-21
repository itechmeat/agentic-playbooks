// @vitest-environment happy-dom
// The process_env part of a profile save, on the page itself: an untouched
// profile sends no `process_env` (the server keeps the stored value), a
// changed policy sends the new one, and an edited allowlist sends its names.
// The api module is doubled; everything else renders for real.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import ProfileEdit from './ProfileEdit.svelte'

const PROFILE_YAML = [
  'executor:',
  '  agent: claude',
  '  model: opus',
  'process_env:',
  '  policy: allowlist',
  '  allow: [GH_TOKEN]',
  '',
].join('\n')

const api = vi.hoisted(() => ({
  fetchProfile: vi.fn(),
  fetchProjects: vi.fn(() => Promise.resolve([])),
  fetchModelCatalog: vi.fn(() =>
    Promise.resolve({ agents: [], models: [], options_by_agent: {}, cloud: [] }),
  ),
  fetchSkills: vi.fn(() => Promise.resolve([])),
  refreshModelCatalog: vi.fn(),
  writeProfile: vi.fn(() => Promise.resolve({ warnings: [], skills_unapproved: [] })),
}))
vi.mock('../lib/api', () => api)
const toast = vi.hoisted(() => ({ error: vi.fn(), warning: vi.fn(), success: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  api.fetchProfile.mockReset()
  api.fetchProfile.mockImplementation(() =>
    Promise.resolve({ profile_yaml: PROFILE_YAML, soul_md: '', profile_digest: 'sha256:d' }),
  )
  api.writeProfile.mockClear()
  toast.error.mockClear()
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const tick = async () => {
  flushSync()
  await new Promise((r) => setTimeout(r))
  flushSync()
}

async function mountEditor() {
  app = mount(ProfileEdit, { target: document.body, props: { name: 'p', scope: 'global' } })
  await tick()
  await tick()
}

async function save(): Promise<Record<string, unknown>> {
  const button = [...document.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Save')
  expect(button, 'the Save button').toBeTruthy()
  button!.click()
  await tick()
  expect(toast.error).not.toHaveBeenCalled()
  const calls = api.writeProfile.mock.calls as unknown as [Record<string, unknown>, string][]
  expect(calls.length).toBeGreaterThan(0)
  return calls[calls.length - 1][0]
}

function choose(policy: 'inherit' | 'allowlist') {
  const select = document.querySelector<HTMLSelectElement>('#pf-env-policy')!
  select.value = policy
  select.dispatchEvent(new Event('change', { bubbles: true }))
  flushSync()
}

describe('ProfileEdit process_env', () => {
  it('sends no process_env for an untouched profile', async () => {
    await mountEditor()
    expect(document.querySelector<HTMLSelectElement>('#pf-env-policy')!.value).toBe('allowlist')
    const body = await save()
    expect('process_env' in body && body.process_env !== undefined).toBe(false)
    expect(JSON.parse(JSON.stringify(body))).not.toHaveProperty('process_env')
  })

  it('sends the new policy once it is changed', async () => {
    await mountEditor()
    choose('inherit')
    const body = await save()
    expect(body.process_env).toEqual({ policy: 'inherit' })
  })

  it('sends the edited allowlist names', async () => {
    await mountEditor()
    const text = document.querySelector<HTMLTextAreaElement>('#pf-env-allow')!
    text.value = 'GH_TOKEN\nAWS_*'
    text.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    const body = await save()
    expect(body.process_env).toEqual({ policy: 'allowlist', allow: ['GH_TOKEN', 'AWS_*'] })
  })
})
