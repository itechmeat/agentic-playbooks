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
    expect(resumeRun).toHaveBeenCalledWith('run-1', 'w', { consentNonce: undefined, budget: undefined })
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
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', {
      consentNonce: 'consent-abc',
      budget: undefined,
    })
  })

  it('resumes a budget pause through its form with a raise or a run once', async () => {
    resumeRun.mockResolvedValue({ run_id: 'run-1', resumed_from: 'b', detached: true })
    show({
      status: 'paused',
      pause: { ...paused, by: 'engine', reason: 'budget (tokens: 100 left, `b` needs ~200)', cause: 'budget' },
      budget: {
        limit: { tokens: 300 },
        on_no_estimate: 'ask',
        sets: 1,
        tokens: { spent: 200, unknown_attempts: 0, remaining: 100 },
        paused: { node: 'b', basis: 'median', need: { tokens: 200 }, remaining: { tokens: 100 } },
      },
    })
    expect(byTestId('run-pause-cause')?.textContent).toBe('by its budget')
    byTestId('run-resume-button')?.click()
    flushSync()
    expect(resumeRun).not.toHaveBeenCalled()
    const form = byTestId('run-budget-resume-form')
    expect(form?.textContent).toContain('paused before b')
    form?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    flushSync()
    expect(byTestId('run-budget-error')?.textContent).toContain('raise or set a limit')
    expect(resumeRun).not.toHaveBeenCalled()
    const raise = byTestId('run-budget-raise-tokens') as HTMLInputElement
    raise.value = '50k'
    raise.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    form?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    await tick()
    expect(resumeRun).toHaveBeenCalledWith('run-1', 'w', {
      consentNonce: undefined,
      budget: { raise_tokens: 50000 },
    })
  })

  function input(id: string, value: string) {
    const el = byTestId(id) as HTMLInputElement
    el.value = value
    el.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
  }

  function submitBudget() {
    byTestId('run-budget-resume-form')?.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
  }

  it('opens the budget form on a budget_paused refusal and sends a run once', async () => {
    resumeRun.mockRejectedValueOnce(new ApiError('refused', 409, 'budget_paused'))
    resumeRun.mockResolvedValueOnce({ run_id: 'run-1', resumed_from: 'b', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    expect(byTestId('run-budget-resume-form')).not.toBeNull()
    expect(byTestId('run-budget-error')?.textContent).toContain('the budget paused this run')
    byTestId('run-budget-once')?.click()
    flushSync()
    submitBudget()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', { consentNonce: undefined, budget: { once: true } })
  })

  it('Resume closes a budget form a refusal opened on another pause', async () => {
    resumeRun.mockRejectedValueOnce(new ApiError('refused', 409, 'budget_paused'))
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    expect(byTestId('run-budget-resume-form')).not.toBeNull()
    byTestId('run-resume-button')?.click()
    flushSync()
    expect(byTestId('run-budget-resume-form')).toBeNull()
    expect(resumeRun).toHaveBeenCalledTimes(1)
  })

  it('sets a new limit and the policy from the form', async () => {
    resumeRun.mockRejectedValueOnce(new ApiError('refused', 409, 'budget_paused'))
    resumeRun.mockResolvedValueOnce({ run_id: 'run-1', resumed_from: 'b', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    input('run-budget-set-tokens', '500k')
    byTestId('run-budget-policy-continue')?.click()
    flushSync()
    submitBudget()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', {
      consentNonce: undefined,
      budget: { tokens: 500_000, on_no_estimate: 'continue' },
    })
  })

  it('a failed budget resume leaves no change behind for a later plain resume', async () => {
    resumeRun.mockRejectedValueOnce(new ApiError('refused', 409, 'budget_paused'))
    resumeRun.mockRejectedValueOnce(new ApiError('busy', 409, 'workdir_busy', 'held'))
    resumeRun.mockResolvedValueOnce({ run_id: 'run-1', resumed_from: 'b', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    input('run-budget-raise-tokens', '50k')
    submitBudget()
    await tick()
    flushSync()
    // The Resume button toggles the form closed, then a plain resume.
    byTestId('run-resume-button')?.click()
    flushSync()
    expect(byTestId('run-budget-resume-form')).toBeNull()
    byTestId('run-resume-button')?.click()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', { consentNonce: undefined, budget: undefined })
  })

  it("the consent retry of a budget resume carries the budget change", async () => {
    resumeRun.mockRejectedValueOnce(new ApiError('refused', 409, 'budget_paused'))
    resumeRun.mockRejectedValueOnce(
      new ApiError('refused', 409, 'irreversible_requires_confirmation', undefined, {
        sources: ['node pr'],
        consent_nonce: 'consent-abc',
      }),
    )
    resumeRun.mockResolvedValueOnce({ run_id: 'run-1', resumed_from: 'b', detached: true })
    show({ status: 'paused', pause: paused })
    byTestId('run-resume-button')?.click()
    await tick()
    flushSync()
    input('run-budget-raise-tokens', '50k')
    submitBudget()
    await tick()
    flushSync()
    byTestId('irreversible-confirm')?.click()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('run-1', 'w', {
      consentNonce: 'consent-abc',
      budget: { raise_tokens: 50000 },
    })
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
