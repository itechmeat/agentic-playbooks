// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { fetchNotify, postRunNotify } = vi.hoisted(() => ({ fetchNotify: vi.fn(), postRunNotify: vi.fn() }))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, fetchNotify, postRunNotify }
})
vi.mock('svelte-sonner', () => ({ toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn() } }))

import { toast } from 'svelte-sonner'
import { ApiError } from './api/http'
import RunNotifyControl from './RunNotifyControl.svelte'
import type { MachineNotify, RunNotify } from './api.gen'

const target = { connector: 'telegram', account: 'me', function: 'send_message', to: '42' }
const machine: MachineNotify = { target, on: ['review', 'question', 'paused', 'finished'], default_on: false }
const on: RunNotify = {
  active: true,
  target,
  on: ['review', 'finished'],
  source: 'run',
  sent: [{ transition: 'review.12', state: 'confirmed', at: 1 }],
  problems: [],
}

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  fetchNotify.mockReset()
  postRunNotify.mockReset()
  vi.mocked(toast.success).mockClear()
  vi.mocked(toast.error).mockClear()
  vi.mocked(toast.warning).mockClear()
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const tick = () => new Promise((r) => setTimeout(r, 0))

async function show(props: Record<string, unknown>) {
  app = mount(RunNotifyControl, {
    target: document.body,
    props: { id: 'run-1', workspace: 'w', status: 'running', ...props } as never,
  })
  flushSync()
  await tick()
  flushSync()
}

describe('RunNotifyControl', () => {
  it('shows where a live run notifies, what was sent, and turns it off', async () => {
    fetchNotify.mockResolvedValue(machine)
    postRunNotify.mockResolvedValue({ run_id: 'run-1' })
    const onchange = vi.fn()
    await show({ notify: on, onchange })
    expect(byTestId('run-notify-state')?.textContent).toBe('on')
    expect(byTestId('run-notify-target')?.textContent).toBe('telegram/me send_message to 42')
    expect(byTestId('run-notify-sent')?.textContent).toBe('review.12: sent')
    expect(byTestId('run-notify-on')).toBeNull()
    byTestId('run-notify-off')!.click()
    await tick()
    expect(postRunNotify).toHaveBeenCalledWith('run-1', false, 'w')
    expect(onchange).toHaveBeenCalled()
  })

  it('offers Notify me when the machine has a target, and shows a refusal', async () => {
    fetchNotify.mockResolvedValue(machine)
    postRunNotify.mockRejectedValue(
      new ApiError('refused', 409, 'notify_unavailable', 'connector `telegram` is not approved'),
    )
    await show({ notify: null })
    expect(byTestId('run-notify-state')?.textContent).toBe('off')
    byTestId('run-notify-on')!.click()
    await tick()
    expect(postRunNotify).toHaveBeenCalledWith('run-1', true, 'w')
    expect(toast.error).toHaveBeenCalledWith('Notifications cannot be sent', {
      description: 'connector `telegram` is not approved',
    })
  })

  it('has no button without a target, and shows a problem a default had', async () => {
    fetchNotify.mockResolvedValue({ on: [], default_on: false })
    await show({ notify: { active: false, on: [], sent: [], problems: ['connector `telegram` is not approved'] } })
    expect(byTestId('run-notify-problem')?.textContent).toBe('connector `telegram` is not approved')
    expect(byTestId('run-notify-on')).toBeNull()
    expect(byTestId('run-notify-off')).toBeNull()
  })

  it('shows the warnings of a change that went through', async () => {
    fetchNotify.mockResolvedValue(machine)
    postRunNotify.mockResolvedValue({ run_id: 'run-1', warnings: ['the project narrows it'] })
    await show({ notify: null })
    byTestId('run-notify-on')!.click()
    await tick()
    expect(toast.warning).toHaveBeenCalledWith('the project narrows it')
    expect(toast.success).toHaveBeenCalledWith('Notifications are on for this run')
  })

  it('says when turning notifications off failed', async () => {
    fetchNotify.mockResolvedValue(machine)
    postRunNotify.mockRejectedValue(new ApiError('failed', 500, 'notify_failed', 'the journal is locked'))
    await show({ notify: on })
    byTestId('run-notify-off')!.click()
    await tick()
    expect(toast.error).toHaveBeenCalledWith('Could not turn notifications off', {
      description: 'the journal is locked',
    })
  })

  it('shows settings that do not read instead of claiming there is no target', async () => {
    fetchNotify.mockResolvedValue({ on: [], default_on: false, problem: 'invalid global config' })
    await show({ notify: null })
    expect(byTestId('run-notify-machine-problem')?.textContent).toBe('invalid global config')
    expect(byTestId('run-notify-no-target')).toBeNull()
    unmount(app!)
    app = null
    document.body.innerHTML = ''

    fetchNotify.mockRejectedValue(new Error('offline'))
    await show({ notify: null })
    expect(byTestId('run-notify-machine-problem')?.textContent).toBe(
      'The notification settings could not be read.',
    )
    expect(byTestId('run-notify-on')).toBeNull()
  })

  it('says nothing about the target until the settings are read', async () => {
    let answer: (m: MachineNotify) => void = () => {}
    fetchNotify.mockReturnValue(new Promise<MachineNotify>((r) => (answer = r)))
    await show({ notify: null })
    expect(byTestId('run-notify-no-target')).toBeNull()
    expect(byTestId('run-notify-machine-problem')).toBeNull()
    expect(byTestId('run-notify-on')).toBeNull()
    answer({ on: [], default_on: false })
    await tick()
    flushSync()
    expect(byTestId('run-notify-no-target')).not.toBeNull()
  })

  it('shows no buttons on a finished run and nothing about a run that never notified', async () => {
    fetchNotify.mockResolvedValue(machine)
    await show({ notify: on, status: 'succeeded' })
    expect(byTestId('run-notify-state')?.textContent).toBe('on')
    expect(byTestId('run-notify-off')).toBeNull()
    unmount(app!)
    app = null
    document.body.innerHTML = ''
    await show({ notify: null, status: 'succeeded' })
    expect(byTestId('run-notify-state')).toBeNull()
  })
})
