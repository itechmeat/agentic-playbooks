// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import MakeCurrentDialog from './MakeCurrentDialog.svelte'
import { ApiError } from './api'
import { moveOutcome, type MoveOutcome } from './currentmove'
import type { Moved } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)

const moved: Moved = {
  change_id: 'c1',
  from: '1.0.0',
  to: '1.1.0',
  superseded_candidate: null,
  awaiting_approval: false,
  audit_pending: false,
}

async function settle() {
  for (let i = 0; i < 5; i++) await tick()
  flushSync()
}

function open(submit: (reason: string, expected: string | null) => Promise<MoveOutcome>, extra: Record<string, unknown> = {}) {
  app = mount(MakeCurrentDialog, {
    target: document.body,
    props: { open: true, playbookId: 'demo', from: '1.0.0', to: '1.1.0', submit, ...extra },
  })
  flushSync()
}

describe('MakeCurrentDialog', () => {
  it('shows the move and posts the reason the person typed', async () => {
    const submit = vi.fn(async () => ({ kind: 'moved', moved }) as MoveOutcome)
    const ondone = vi.fn()
    open(submit, { ondone })
    expect(byTestId('make-current-move')?.textContent?.replace(/\s+/g, ' ').trim()).toBe(
      '1.0.0 -> 1.1.0',
    )
    const reason = byTestId('make-current-reason') as HTMLTextAreaElement
    reason.value = '  the retry loops  '
    reason.dispatchEvent(new Event('input', { bubbles: true }))
    flushSync()
    byTestId('make-current-confirm')?.click()
    await settle()
    expect(submit).toHaveBeenCalledWith('the retry loops', '1.0.0')
    expect(ondone).toHaveBeenCalledWith(moved)
  })

  it('on a stale answer says so, reloads the page and stays open', async () => {
    const submit = vi.fn(async () => ({ kind: 'stale', actual: '1.2.0' }) as MoveOutcome)
    const onstale = vi.fn()
    const ondone = vi.fn()
    open(submit, { onstale, ondone })
    byTestId('make-current-confirm')?.click()
    await settle()
    expect(onstale).toHaveBeenCalledTimes(1)
    expect(ondone).not.toHaveBeenCalled()
    expect(byTestId('make-current-note')?.textContent).toContain('it is now 1.2.0')
    expect(byTestId('make-current-dialog')).not.toBeNull()
  })

  it('stays busy until the reload after a stale answer is done', async () => {
    const submit = vi.fn(async () => ({ kind: 'stale', actual: '1.2.0' }) as MoveOutcome)
    let reloaded: () => void = () => {}
    const onstale = vi.fn(() => new Promise<void>((r) => (reloaded = r)))
    open(submit, { onstale })
    const confirm = byTestId('make-current-confirm') as HTMLButtonElement
    confirm.click()
    await settle()
    expect(onstale).toHaveBeenCalledTimes(1)
    expect(confirm.disabled).toBe(true)
    expect(byTestId('make-current-note')).toBeNull()
    confirm.click()
    await settle()
    expect(submit).toHaveBeenCalledTimes(1)
    reloaded()
    await settle()
    expect(confirm.disabled).toBe(false)
    expect(byTestId('make-current-note')?.textContent).toContain('it is now 1.2.0')
  })

  it('offers no move to the version that is already current', async () => {
    const submit = vi.fn(async () => ({ kind: 'moved', moved }) as MoveOutcome)
    open(submit, { from: '1.1.0' })
    const confirm = byTestId('make-current-confirm') as HTMLButtonElement
    expect(confirm.disabled).toBe(true)
    expect(byTestId('make-current-arrived')?.textContent).toContain('already the current version')
    confirm.click()
    await settle()
    expect(submit).not.toHaveBeenCalled()
  })

  it('shows a refusal and moves nothing', async () => {
    const submit = vi.fn(
      async () => ({ kind: 'refused', message: 'the playbook is frozen' }) as MoveOutcome,
    )
    const ondone = vi.fn()
    open(submit, { ondone })
    byTestId('make-current-confirm')?.click()
    await settle()
    expect(byTestId('make-current-note')?.textContent).toContain('frozen')
    expect(ondone).not.toHaveBeenCalled()
  })
})

describe('moveOutcome', () => {
  it('reads a 409 stale answer with what current holds now', async () => {
    const out = await moveOutcome(async () => {
      throw new ApiError('x', 409, 'stale', undefined, { error: 'stale', actual: '1.2.0' })
    })
    expect(out).toEqual({ kind: 'stale', actual: '1.2.0' })
  })

  it('reads any other refusal by its message', async () => {
    const out = await moveOutcome(async () => {
      throw new ApiError('x', 409, 'is_candidate', undefined, {
        error: 'is_candidate',
        message: '1.2.0 is the candidate on trial',
      })
    })
    expect(out).toEqual({ kind: 'refused', message: '1.2.0 is the candidate on trial' })
  })
})
