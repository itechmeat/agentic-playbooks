// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'

const { postAnswer, postReview, resolveIntent, resumeRun } = vi.hoisted(() => ({
  postAnswer: vi.fn(),
  postReview: vi.fn(),
  resolveIntent: vi.fn(),
  resumeRun: vi.fn(),
}))
vi.mock('./api', async () => {
  const http = await import('./api/http')
  return { ApiError: http.ApiError, postAnswer, postReview, resolveIntent, resumeRun }
})
vi.mock('svelte-sonner', () => ({ toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn() } }))

import NextActionsPanel from './NextActionsPanel.svelte'
import { ApiError } from './api/http'
import { auth } from './auth.svelte'
import { pageControls } from './nextactions'
import type { NextAction, RunActions } from './api.gen'

const review = (over: Partial<NextAction> = {}): NextAction => ({
  code: 'decide_review',
  run_id: 'parent',
  target: { node: 'gate', request: { kind: 'review', seq: 7 } },
  args: [{ name: 'decision', value: { kind: 'choice', options: ['approve', 'reject'] }, required: true }],
  principals: ['person', 'operator'],
  ready: true,
  reason: 'node `gate` waits for a review',
  ...over,
})

const intent: NextAction = {
  code: 'resolve_intent',
  run_id: 'parent',
  target: { node: 'send' },
  args: [
    { name: 'intent', value: { kind: 'given', value: 'int-1' }, required: true },
    { name: 'decision', value: { kind: 'choice', options: ['sent', 'not_sent'] }, required: true },
  ],
  principals: ['person'],
  ready: true,
  reason: 'nobody can tell whether the send of intent `int-1` went out',
}

const report: NextAction = {
  code: 'read_report',
  run_id: 'parent',
  target: {},
  args: [],
  principals: ['person', 'operator', 'host', 'supervisor'],
  ready: true,
  reason: 'the run finished (succeeded); read its report',
  command: 'apb runs parent',
}

const runActions = (actions: NextAction[], over: Partial<RunActions> = {}): RunActions => ({
  observed_at_ms: 0,
  status: 'running',
  stop: 'needs_input',
  actions,
  primary: 0,
  ...over,
})

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  for (const f of [postAnswer, postReview, resolveIntent, resumeRun]) f.mockReset()
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const allByTestId = (id: string) => [...document.querySelectorAll<HTMLElement>(`[data-testid="${id}"]`)]
const tick = () => new Promise((r) => setTimeout(r, 0))

function show(props: Record<string, unknown>) {
  app = mount(NextActionsPanel, { target: document.body, props: { runId: 'parent', workspace: 'w', ...props } as never })
  flushSync()
}

describe('NextActionsPanel', () => {
  it('renders nothing without actions', () => {
    show({ actions: runActions([]) })
    expect(byTestId('next-actions')).toBeNull()
  })

  it('marks the primary action and shows a copyable line for what the page does not take', () => {
    const wait: NextAction = { ...report, code: 'wait_again', reason: 'wait again', command: 'apb wait parent' }
    show({ actions: runActions([review(), wait], { primary: 0 }) })
    const items = allByTestId('next-action')
    expect(items).toHaveLength(2)
    expect(items[0].querySelector('[data-testid="next-action-primary"]')).not.toBeNull()
    expect(items[1].querySelector('[data-testid="next-action-primary"]')).toBeNull()
    expect(items[1].querySelector('[data-testid="next-action-cli"]')?.textContent).toContain('apb wait parent')
  })

  it('posts a review decision with its request seq', async () => {
    const onchange = vi.fn()
    postReview.mockResolvedValue({ posted_seq: 8 })
    show({ actions: runActions([review()]), onchange })
    allByTestId('next-action-option')[0].click()
    await tick()
    expect(postReview).toHaveBeenCalledWith('parent', 'gate', 'approve', '', 'w', undefined, 7)
    expect(onchange).toHaveBeenCalled()
  })

  it("posts a child's action to the child's run, and says it acts on a sub-playbook run", async () => {
    postAnswer.mockResolvedValue({ posted_seq: 3 })
    const child: NextAction = {
      code: 'answer_question',
      run_id: 'child-1',
      target: { node: 'ask', request: { kind: 'question', seq: 12 } },
      args: [{ name: 'answer', value: { kind: 'choice', options: ['yes', 'no'], default: 'no' }, required: true }],
      principals: ['person'],
      ready: true,
      reason: 'sub-playbook node `sub`: node `ask` asks',
    }
    show({ actions: runActions([child]) })
    expect(byTestId('next-action-child')?.textContent).toContain('child-1')
    allByTestId('next-action-option')[0].click()
    await tick()
    expect(postAnswer).toHaveBeenCalledWith('child-1', { answer: 'yes', node: 'ask', request: 12 }, 'w')
  })

  it('posts a publish decision with its nonce and request', async () => {
    postReview.mockResolvedValue({ posted_seq: 1 })
    const publish = review({
      code: 'decide_publish',
      target: { node: 'ship', request: { kind: 'publish', seq: 9, nonce: 'n-1' } },
      args: [
        { name: 'decision', value: { kind: 'choice', options: ['publish', 'skip'] }, required: true },
        { name: 'nonce', value: { kind: 'given', value: 'n-1' }, required: true },
      ],
    })
    show({ actions: runActions([publish]) })
    const skip = allByTestId('next-action-option').find((b) => b.textContent?.trim() === 'skip')!
    skip.click()
    await tick()
    expect(postReview).toHaveBeenCalledWith('parent', 'ship', 'skip', '', 'w', 'n-1', 9)
  })

  it('disables a not-ready action and says why, and shows when it continues by itself', () => {
    const a = review({
      ready: false,
      not_ready: { code: 'paused', detail: 'the run is paused' },
      auto: { kind: 'frozen', remaining_ms: 125_000, outcome: 'answered_default' },
    })
    show({ actions: runActions([a]) })
    expect(byTestId('next-action-not-ready')?.textContent).toContain('the run is paused')
    expect(byTestId('next-action-auto')?.textContent).toBe(
      'timer frozen while paused, 2m 5s left: then its default answer is posted',
    )
    expect(allByTestId('next-action-option').every((b) => (b as HTMLButtonElement).disabled)).toBe(true)
  })

  it('says under each action what an unattended run does at its wait, and nothing where it does not apply', () => {
    const gate = review({ unattended: { kind: 'fails_run', code: 'hitl_required' } })
    const decided = review({ target: { node: 'ship' }, unattended: { kind: 'decides', option: 'approve' } })
    const retry: NextAction = {
      code: 'retry_now',
      run_id: 'parent',
      target: { node: 'flaky' },
      args: [],
      principals: ['person', 'operator'],
      ready: true,
      reason: 'node `flaky` waits out its retry delay',
      unattended: { kind: 'proceeds', auto: 'retry_attempt' },
    }
    show({ actions: runActions([gate, decided, retry, report], { stop: 'needs_input' }) })
    const lines = allByTestId('next-action-unattended')
    expect(lines.map((l) => l.textContent?.trim())).toEqual([
      'unattended: fails the run (hitl_required)',
      'unattended: decides `approve` at once',
      "unattended: goes on by itself (the node's retry starts)",
    ])
    expect(lines.map((l) => l.classList.contains('stops'))).toEqual([true, false, false])
    expect(allByTestId('next-action').length).toBe(4)
  })

  it('shows the intent buttons on a finished run, confirms the consequence, and resolves', async () => {
    const onchange = vi.fn()
    resolveIntent.mockResolvedValue({ run_id: 'parent', intent: 'int-1', settled: 'not_sent' })
    show({ actions: runActions([intent, report], { status: 'succeeded', stop: 'finished' }), onchange })
    expect(byTestId('intent-sent')?.textContent?.trim()).toBe('It went out')
    byTestId('intent-not-sent')!.click()
    flushSync()
    await tick()
    expect(byTestId('intent-dialog')?.textContent).toContain('a repeat of this send sends it again')
    expect(resolveIntent).not.toHaveBeenCalled()
    byTestId('intent-confirm')!.click()
    await tick()
    expect(resolveIntent).toHaveBeenCalledWith('parent', 'int-1', false, '', 'w')
    expect(onchange).toHaveBeenCalled()
  })

  it('states the consequence of a send that went out', async () => {
    show({ actions: runActions([intent], { status: 'failed', stop: 'finished' }) })
    byTestId('intent-sent')!.click()
    flushSync()
    await tick()
    expect(byTestId('intent-consequence')?.textContent).toBe(
      'a repeat of this send answers already_sent without sending',
    )
  })

  it('drops the intent action once the run no longer lists it', async () => {
    // The page reloads the run after the decision and renders again with
    // what the server lists now.
    resolveIntent.mockResolvedValue({ run_id: 'parent', intent: 'int-1', settled: 'confirmed' })
    const reload = vi.fn(() => {
      unmount(app!)
      document.body.innerHTML = ''
      show({ actions: runActions([report], { status: 'succeeded', stop: 'finished' }) })
    })
    show({ actions: runActions([intent, report], { status: 'succeeded', stop: 'finished' }), onchange: reload })
    byTestId('intent-sent')!.click()
    flushSync()
    await tick()
    byTestId('intent-confirm')!.click()
    await tick()
    await tick()
    expect(reload).toHaveBeenCalled()
    expect(byTestId('intent-sent')).toBeNull()
    expect(allByTestId('next-action')).toHaveLength(1)
    expect(byTestId('next-action-cli')?.textContent).toContain('apb runs parent')
  })

  it('offers the resume of a stopped run', async () => {
    resumeRun.mockResolvedValue({ run_id: 'parent' })
    const resume: NextAction = { ...report, code: 'resume_run', reason: 'the run is paused; resume it' }
    show({ actions: runActions([resume], { status: 'paused', stop: 'stopped' }) })
    byTestId('next-action-resume')!.click()
    await tick()
    expect(resumeRun).toHaveBeenCalledWith('parent', 'w', { consentNonce: undefined })
  })

  it('starts a waiting retry now with its node', async () => {
    resumeRun.mockResolvedValue({ run_id: 'parent', retry_now: ['work'], live: true })
    const retry: NextAction = {
      code: 'retry_now',
      run_id: 'parent',
      target: { node: 'work' },
      args: [{ name: 'node', value: { kind: 'given', value: 'work' }, required: true }],
      principals: ['person', 'operator', 'supervisor'],
      ready: true,
      auto: { kind: 'frozen', remaining_ms: 90_000, outcome: 'retry_attempt' },
      reason: 'node `work` waits to retry; it starts by itself, or start it now',
    }
    show({ actions: runActions([retry], { status: 'running', stop: 'progresses_by_itself' }) })
    expect(byTestId('next-action-auto')!.textContent).toContain('delay frozen while paused, 1m 30s left')
    expect(byTestId('next-action-auto')!.textContent).toContain("the node's retry starts")
    expect(byTestId('next-action-cli')).toBeNull()
    byTestId('next-action-retry-now')!.click()
    await tick()
    expect(resumeRun).toHaveBeenCalledWith('parent', 'w', {
      consentNonce: undefined,
      launchNonce: undefined,
      retryNow: { node: 'work' },
    })
  })

  it('replays a retry-now resume with its node after the launch confirmation', async () => {
    resumeRun
      .mockRejectedValueOnce(
        new ApiError('refused', 409, 'launch_needs_confirmation', 'confirm', {
          policy: 'launch_needs_confirmation',
          detail: 'confirm the launch',
          launch_nonce: 'n-1',
        }),
      )
      .mockResolvedValueOnce({ run_id: 'parent', retry_now: ['work'] })
    const retry: NextAction = {
      code: 'retry_now',
      run_id: 'parent',
      target: { node: 'work' },
      args: [{ name: 'node', value: { kind: 'given', value: 'work' }, required: true }],
      principals: ['person'],
      ready: true,
      reason: 'node `work` waits to retry',
    }
    show({ actions: runActions([retry], { status: 'running', stop: 'progresses_by_itself' }) })
    byTestId('next-action-retry-now')!.click()
    await tick()
    flushSync()
    expect(resumeRun).toHaveBeenCalledTimes(1)
    byTestId('launch-confirm-ok')!.click()
    await tick()
    await tick()
    expect(resumeRun).toHaveBeenCalledTimes(2)
    expect(resumeRun).toHaveBeenLastCalledWith('parent', 'w', {
      consentNonce: undefined,
      launchNonce: 'n-1',
      retryNow: { node: 'work' },
    })
  })

  it('replays a retry-now resume with its node after the irreversible consent, and says the run resumed', async () => {
    resumeRun
      .mockRejectedValueOnce(
        new ApiError('refused', 409, 'irreversible_requires_confirmation', 'confirm', {
          sources: ['node `push` declares irreversible'],
          consent_nonce: 'c-1',
        }),
      )
      .mockResolvedValueOnce({ run_id: 'parent', retry_now: ['work'], detached: true })
    const retry: NextAction = {
      code: 'retry_now',
      run_id: 'parent',
      target: { node: 'work' },
      args: [{ name: 'node', value: { kind: 'given', value: 'work' }, required: true }],
      principals: ['person'],
      ready: true,
      reason: 'node `work` waits to retry',
    }
    show({ actions: runActions([retry], { status: 'running', stop: 'progresses_by_itself' }) })
    byTestId('next-action-retry-now')!.click()
    await tick()
    flushSync()
    byTestId('irreversible-confirm')!.click()
    await tick()
    await tick()
    expect(resumeRun).toHaveBeenLastCalledWith('parent', 'w', {
      consentNonce: 'c-1',
      launchNonce: undefined,
      retryNow: { node: 'work' },
    })
    const { toast } = await import('svelte-sonner')
    expect(toast.success).toHaveBeenLastCalledWith('Run resumed with the retry due')
  })

  it("leaves out what another control on the page takes, and keeps a child's and what has none", () => {
    const question: NextAction = {
      code: 'answer_question',
      run_id: 'parent',
      target: { node: 'ask', request: { kind: 'question', seq: 3 } },
      args: [{ name: 'answer', value: { kind: 'supplied', hint: 'your answer' }, required: true }],
      principals: ['person'],
      ready: true,
      reason: 'node `ask` asks',
    }
    const resume: NextAction = { ...report, code: 'resume_run', reason: 'resume it' }
    const childReview = review({ run_id: 'child-1' })
    const controls = pageControls({ questions: [{ node: 'ask' }], reviews: [{ node: 'gate' }], publishes: [] }, true)
    show({ actions: runActions([review(), question, resume, childReview, intent]), controls })
    const codes = allByTestId('next-action').map((li) => li.dataset.code)
    expect(codes).toEqual(['decide_review', 'resolve_intent'])
    // The review left is the child's: the review card covers only the page's own run.
    expect(byTestId('next-action-child')?.textContent).toContain('child-1')
    expect(byTestId('next-action-resume')).toBeNull()
    expect(byTestId('intent-sent')).not.toBeNull()
  })

  it("offers no publish consent for a sub-playbook run's plan, only skip and the command line", () => {
    const publish = review({
      code: 'decide_publish',
      run_id: 'child-1',
      target: { node: 'ship', request: { kind: 'publish', seq: 9, nonce: 'n-1' } },
      args: [
        { name: 'decision', value: { kind: 'choice', options: ['publish', 'skip'] }, required: true },
        { name: 'nonce', value: { kind: 'given', value: 'n-1' }, required: true },
      ],
      command: "apb review '--decision=<publish|skip>' --nonce=n-1 --request=publish:9 child-1 ship",
    })
    show({ actions: runActions([publish]) })
    const options = allByTestId('next-action-option').map((b) => b.textContent?.trim())
    expect(options).toEqual(['skip'])
    expect(byTestId('next-action-cli')?.textContent).toContain('child-1 ship')
  })

  it('keeps a typed answer when its post fails and clears it once one goes through', async () => {
    const free: NextAction = {
      code: 'answer_question',
      run_id: 'parent',
      target: { node: 'ask', request: { kind: 'question', seq: 3 } },
      args: [{ name: 'answer', value: { kind: 'supplied', hint: 'your answer' }, required: true }],
      principals: ['person'],
      ready: true,
      reason: 'node `ask` asks',
    }
    const onchange = vi.fn()
    show({ actions: runActions([free]), onchange })
    const input = document.querySelector<HTMLInputElement>('input[aria-label="Answer"]')!
    input.value = 'blue'
    input.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    postAnswer.mockRejectedValueOnce(new ApiError('conflict', 409, 'conflict', 'the request changed'))
    byTestId('next-action-send')!.click()
    await tick()
    await tick()
    expect(input.value).toBe('blue')
    expect(onchange).toHaveBeenCalled()
    postAnswer.mockResolvedValueOnce({ posted_seq: 4 })
    byTestId('next-action-send')!.click()
    await tick()
    await tick()
    flushSync()
    expect(postAnswer).toHaveBeenLastCalledWith('parent', { answer: 'blue', node: 'ask', request: 3 }, 'w')
    expect(input.value).toBe('')
  })

  it("names the sub-playbook run when resuming a child asks for irreversible consent", async () => {
    resumeRun.mockRejectedValueOnce(
      new ApiError('refused', 409, 'irreversible_requires_confirmation', 'confirm', {
        sources: ['node `push` declares irreversible'],
        consent_nonce: 'c-1',
      }),
    )
    const resume: NextAction = { ...report, run_id: 'child-1', code: 'resume_run', reason: 'resume it' }
    show({ actions: runActions([resume], { status: 'paused', stop: 'stopped' }), playbook: 'parent-pb' })
    byTestId('next-action-resume')!.click()
    await tick()
    flushSync()
    expect(byTestId('irreversible-subrun')?.textContent).toBe('child-1')
  })

  it('a keyless dashboard shows the command line for a send decision and offers no publish consent', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    try {
      const publish = review({
        code: 'decide_publish',
        target: { node: 'ship', request: { kind: 'publish', seq: 9, nonce: 'n-1' } },
        args: [
          { name: 'decision', value: { kind: 'choice', options: ['publish', 'skip'] }, required: true },
          { name: 'nonce', value: { kind: 'given', value: 'n-1' }, required: true },
        ],
        command: "apb review '--decision=<publish|skip>' --nonce=n-1 --request=publish:9 parent ship",
      })
      const line = { ...intent, command: "apb intent resolve '--sent|--not-sent' parent int-1" }
      show({ actions: runActions([publish, line]) })
      expect(byTestId('intent-sent')).toBeNull()
      expect(allByTestId('next-action-option').map((b) => b.textContent?.trim())).toEqual(['skip'])
      const lines = allByTestId('next-action-cli').map((c) => c.textContent ?? '')
      expect(lines.some((l) => l.includes('apb intent resolve'))).toBe(true)
      expect(lines.some((l) => l.includes('parent ship'))).toBe(true)
    } finally {
      auth.set({ required: false, authenticated: true, checked: false })
    }
  })

  it('says the supervisor takes only a supervisor action, and nothing else for an action without a line', () => {
    const abort: NextAction = { ...report, code: 'supervisor_abort', reason: 'abort', command: undefined }
    const wait: NextAction = { ...report, code: 'wait_again', reason: 'wait', command: undefined }
    show({ actions: runActions([abort, wait]) })
    expect(allByTestId('next-action-supervisor')).toHaveLength(1)
    expect(allByTestId('next-action-no-line')).toHaveLength(1)
  })

  it('hides itself when every action has a control elsewhere', () => {
    const controls = pageControls({ questions: [], reviews: [{ node: 'gate' }], publishes: [] }, false)
    show({ actions: runActions([review()]), controls })
    expect(byTestId('next-actions')).toBeNull()
  })
})
