// @vitest-environment happy-dom
// A live run reloads its `notify` block on every journal write: the
// outcome boxes keep a change not applied yet while the filter itself stays
// the same, and follow it when it changes. Runes need the `.svelte.` infix.
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { fetchNotify, postRunNotify } = vi.hoisted(() => ({ fetchNotify: vi.fn(), postRunNotify: vi.fn() }))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, fetchNotify, postRunNotify }
})
vi.mock('svelte-sonner', () => ({ toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn() } }))

import RunNotifyControl from './RunNotifyControl.svelte'
import type { MachineNotify, RunNotify } from './api.gen'
import { NOTIFY_OUTCOMES } from './consts.gen'

const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }
const machine: MachineNotify = { target, on: ['finished'], default_on: false, finished: [...NOTIFY_OUTCOMES] }
const on: RunNotify = {
  active: true,
  target,
  on: ['review', 'finished'],
  finished: [...NOTIFY_OUTCOMES],
  source: 'run',
  sent: [],
  problems: [],
}

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const box = (o: string) => document.querySelector<HTMLInputElement>(`[data-testid="run-notify-outcome-${o}"]`)!
const tick = () => new Promise((r) => setTimeout(r, 0))

describe('RunNotifyControl on a live run', () => {
  it('keeps a change not applied yet through a reload with the same filter', async () => {
    fetchNotify.mockResolvedValue(machine)
    const props = $state({ id: 'run-1', workspace: 'w', status: 'running', notify: on })
    app = mount(RunNotifyControl, { target: document.body, props: props as never })
    flushSync()
    await tick()
    flushSync()
    box('succeeded').click()
    flushSync()
    expect(box('succeeded').checked).toBe(false)

    props.notify = { ...on, finished: [...NOTIFY_OUTCOMES] }
    flushSync()
    expect(box('succeeded').checked).toBe(false)
    const apply = document.querySelector<HTMLButtonElement>('[data-testid="run-notify-filter-apply"]')!
    expect(apply.disabled).toBe(false)

    // A filter that did change reaches the boxes.
    props.notify = { ...on, finished: ['failed'] }
    flushSync()
    expect(box('failed').checked).toBe(true)
    expect(box('needs_look').checked).toBe(false)
  })
})
