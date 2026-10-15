// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { pauseRun, resumeRun } = vi.hoisted(() => ({ pauseRun: vi.fn(), resumeRun: vi.fn() }))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, pauseRun, resumeRun }
})
vi.mock('svelte-sonner', () => ({ toast: { success: vi.fn(), error: vi.fn() } }))

import { toast } from 'svelte-sonner'
import { ApiError } from './api/http'
import RunPauseControls from './RunPauseControls.svelte'
import type { PauseInfo } from './types'

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  pauseRun.mockReset()
  resumeRun.mockReset()
  vi.mocked(toast.success).mockClear()
  vi.mocked(toast.error).mockClear()
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const tick = () => new Promise((r) => setTimeout(r, 0))

function show(props: Record<string, unknown>) {
  app = mount(RunPauseControls, {
    target: document.body,
    props: { id: 'run-1', workspace: 'w', playbookId: 'pb', ...props } as never,
  })
  flushSync()
}

const paused: PauseInfo = {
  reason: 'lunch',
  by: 'operator',
  mode: 'drain',
  since: Date.UTC(2026, 9, 15, 20, 0, 0),
  until: Date.UTC(2026, 9, 15, 22, 0, 0),
}

describe('RunPauseControls', () => {
  it('shows nothing for a finished run', () => {
    show({ status: 'succeeded' })
    expect(byTestId('run-pause-bar')).toBeNull()
  })

  it('offers Pause on a running run and sends drain with the reason and until', async () => {
    pauseRun.mockResolvedValue({ outcome: 'posted' })
    const changed = vi.fn()
    show({ status: 'running', onchange: changed })
    expect(byTestId('run-resume-button')).toBeNull()
    byTestId('run-pause-button')?.click()
    flushSync()
    const reason = byTestId('run-pause-reason') as HTMLInputElement
    reason.value = 'lunch'
    reason.dispatchEvent(new Event('input', { bubbles: true }))
    const until = byTestId('run-pause-until') as HTMLInputElement
    until.value = '90m'
    until.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    byTestId('run-pause-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    await tick()
    expect(pauseRun).toHaveBeenCalledWith(
      'run-1',
      { mode: 'drain', reason: 'lunch', until: '90m' },
      'w',
    )
    expect(changed).toHaveBeenCalled()
  })

  it('shows the pause and a Resume button on a paused run', () => {
    show({ status: 'paused', pause: paused })
    expect(byTestId('run-pause-button')).toBeNull()
    const info = byTestId('run-pause-info')?.textContent ?? ''
    expect(info).toContain('paused')
    expect(info).toContain('lunch')
    expect(info).toContain('by operator')
    expect(info).toContain('until 2026-10-15T22:00:00Z')
    expect(byTestId('run-resume-button')).not.toBeNull()
  })

  it('shows a pausing run without any button', () => {
    show({ status: 'running', pause: { ...paused, since: undefined, pausing: true } })
    expect(byTestId('run-pause-info')?.textContent).toContain('pausing')
    expect(byTestId('run-pause-button')).toBeNull()
    expect(byTestId('run-resume-button')).toBeNull()
  })

  it('resumes a paused run', async () => {
    resumeRun.mockResolvedValue({ run_id: 'run-1', resumed_from: 'n', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    expect(resumeRun).toHaveBeenCalledWith('run-1', 'w', undefined)
  })

  it('asks for consent when the server refuses an irreversible resume, then sends the nonce', async () => {
    resumeRun.mockRejectedValueOnce(
      new ApiError('refused', 409, 'irreversible_requires_confirmation', undefined, {
        sources: ['node pr'],
        consent_nonce: 'consent-abc',
      }),
    )
    resumeRun.mockResolvedValueOnce({ run_id: 'run-1', resumed_from: 'n', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    expect(byTestId('irreversible-dialog')).not.toBeNull()
    expect(resumeRun).toHaveBeenCalledTimes(1)
    byTestId('irreversible-confirm')?.click()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', 'consent-abc')
  })

  it('shows a busy-workdir error when another run holds the lock', async () => {
    resumeRun.mockRejectedValueOnce(
      new ApiError(
        '/api/runs/run-1/resume: workdir_busy',
        409,
        'workdir_busy',
        'another write-run holds the workdir (pid 42)',
      ),
    )
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    expect(toast.success).not.toHaveBeenCalled()
    expect(toast.error).toHaveBeenCalledWith('Cannot resume: the workdir is busy', {
      description: expect.stringContaining('another write-run holds the workdir (pid 42)'),
    })
  })
})
