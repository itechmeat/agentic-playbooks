// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import ScenarioCaptureBox from './ScenarioCaptureBox.svelte'

// The box previews and saves a finished run as a scenario over HTTP: the
// test stubs `fetch` and checks the requests it sends and what it shows of
// each answer.
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

const plain = (text: string, status: number) => new Response(text, { status })

const preview = {
  run_id: 'r-1',
  playbook: 'p',
  version: '1.2.0',
  name: 'green-path',
  yaml: 'schema: 1\nid: green-path\ninstruction: say hi\n',
  digest: 'sha256:cafe',
  redacted: 2,
  warnings: ['the run ran version 1.1.0, which is not current'],
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

async function render() {
  app = mount(ScenarioCaptureBox, { target: document.body, props: { id: 'r-1', workspace: 'w1' } })
  await settle()
}

function openBox() {
  q('capture-open')?.click()
  flushSync()
}

function type(raw: string) {
  const input = q('capture-name') as HTMLInputElement
  input.value = raw
  input.dispatchEvent(new Event('input', { bubbles: true }))
  flushSync()
}

async function previewGreenPath() {
  type('green-path')
  fetchMock.mockResolvedValueOnce(json(preview))
  q('capture-preview')?.click()
  await settle()
}

describe('ScenarioCaptureBox', () => {
  it('checks the name grammar as typed and holds Save back while the name is bad', async () => {
    await render()
    openBox()
    expect(q('capture-name-problem')).toBeNull()
    type('Bad Name')
    expect((q('capture-save') as HTMLButtonElement).disabled).toBe(true)
    expect(q('capture-name-problem')?.textContent).toContain('lowercase')
    type('green-path')
    expect(q('capture-name-problem')).toBeNull()
    expect((q('capture-preview') as HTMLButtonElement).disabled).toBe(false)
    // A valid name alone is not enough: the person has not read a preview.
    expect((q('capture-save') as HTMLButtonElement).disabled).toBe(true)
    type('a'.repeat(65))
    expect((q('capture-save') as HTMLButtonElement).disabled).toBe(true)
    expect(q('capture-name-problem')?.textContent).toContain('64')
  })

  it('previews the capture: the YAML, the warnings, the redacted count and the digest', async () => {
    await render()
    openBox()
    await previewGreenPath()
    expect(fetchMock).toHaveBeenLastCalledWith('/api/runs/r-1/scenario-capture?workspace=w1', expect.anything())
    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({
      name: 'green-path',
      nodes: [],
      save: false,
    })
    expect(q('capture-yaml')?.textContent).toContain('id: green-path')
    expect(q('capture-warnings')?.textContent).toContain('not current')
    expect(q('capture-redacted')?.textContent).toContain('2')
    expect(q('capture-digest')?.textContent).toContain('sha256:cafe')
  })

  it('saves with the digest of the preview the person read and shows the saved path', async () => {
    await render()
    openBox()
    await previewGreenPath()
    fetchMock.mockResolvedValueOnce(json({ ...preview, saved: '/demo/evals/scenarios/green-path.yaml' }))
    q('capture-save')?.click()
    await settle()
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      name: 'green-path',
      nodes: [],
      save: true,
      digest: 'sha256:cafe',
    })
    expect(q('capture-saved')?.textContent).toContain('/demo/evals/scenarios/green-path.yaml')
    // Done: saving the same name again is not offered.
    expect((q('capture-save') as HTMLButtonElement).disabled).toBe(true)
  })

  it('lists every gap of a not_capturable refusal', async () => {
    await render()
    openBox()
    type('green-path')
    fetchMock.mockResolvedValueOnce(
      plain(
        'not_capturable: list_node (review): a list node cannot be replayed; param_withheld: the secret token was withheld',
        409,
      ),
    )
    q('capture-preview')?.click()
    await settle()
    const gaps = all('capture-gap')
    expect(gaps).toHaveLength(2)
    expect(gaps[0].textContent).toContain('list_node (review)')
    expect(gaps[0].textContent).toContain('a list node cannot be replayed')
    expect(gaps[1].textContent).toContain('param_withheld')
    expect(q('capture-yaml')).toBeNull()
  })

  it('surfaces the server sentence of a refusal that is not a gap list', async () => {
    await render()
    openBox()
    type('green-path')
    fetchMock.mockResolvedValueOnce(plain('unknown field `nodess` at line 1 column 1', 422))
    q('capture-preview')?.click()
    await settle()
    expect(q('capture-error')?.textContent).toContain('unknown field `nodess` at line 1 column 1')
    expect(q('capture-gaps')).toBeNull()
  })
})
