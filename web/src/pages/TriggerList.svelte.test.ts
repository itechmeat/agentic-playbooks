// @vitest-environment happy-dom
// TriggerList's rule row shows a rule whose journal the reader refuses:
// the one sentence the list carries in `journal_error`, as a warn badge on
// the row. The api module is doubled; everything else renders for real.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import TriggerList from './TriggerList.svelte'

const api = vi.hoisted(() => ({
  fetchTriggers: vi.fn(),
  removeTrigger: vi.fn(),
}))
vi.mock('$lib/triggers', async (importOriginal) => {
  const actual = await importOriginal<typeof import('$lib/triggers')>()
  return { ...actual, fetchTriggers: api.fetchTriggers, removeTrigger: api.removeTrigger }
})
const auth = vi.hoisted(() => ({
  authState: vi.fn(() => ({ checked: true, required: true })),
}))
vi.mock('$lib/auth.svelte', () => auth)
const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))
vi.mock('svelte-sonner', () => ({ toast }))

const view = (over: Record<string, unknown> = {}) => ({
  name: 'triage',
  enabled: true,
  source: 'inbox github/main [issues]',
  playbook: 'triage-pb',
  version: '1.0.0',
  scope: 'project',
  incarnation: 'inc1',
  seen_through: 0,
  starts_last_hour: 0,
  approved: true,
  needs_approval: false,
  issues: [],
  ...over,
})

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  api.fetchTriggers.mockReset()
})

async function mountList() {
  try {
    app = mount(TriggerList, { target: document.body })
  } catch (e) {
    console.error('MOUNT FAILED:', e)
    throw e
  }
  // The load effect runs after the mount and resolves a promise.
  await vi.waitFor(() => {
    expect(document.body.querySelector('[data-testid="trigger-row"]')).not.toBeNull()
  })
  flushSync()
}

describe('TriggerList', () => {
  it('shows a refused journal on the rule row with the remedy sentence', async () => {
    // `fetchTriggers` answers the rules array (the envelope is its own).
    api.fetchTriggers.mockResolvedValue([
      view({
        journal_error:
          'journal_refused: the trigger journal of `triage` cannot be read (not a regular file); remove the rule and save it again, or delete .apb/trigger-state/log/inc1.jsonl',
      }),
    ])
    await mountList()
    const badge = document.body.querySelector('[data-testid="trigger-journal-refused"]')
    expect(badge, document.body.innerHTML).not.toBeNull()
    // The badge names the state; its tooltip and the open rule's panel
    // carry the whole sentence with the remedy and the path.
    expect(badge?.getAttribute('title')).toContain('remove the rule and save it again')
    const open = document.body.querySelector<HTMLButtonElement>('[data-testid="trigger-row"] button')
    open?.click()
    await vi.waitFor(() => {
      expect(
        document.body.querySelector('[data-testid="trigger-journal-refused-detail"]'),
      ).not.toBeNull()
    })
    const detail = document.body.querySelector('[data-testid="trigger-journal-refused-detail"]')
    expect(detail?.textContent).toContain('remove the rule and save it again')
    expect(detail?.textContent).toContain('.apb/trigger-state/log/inc1.jsonl')
  })

  it('shows no journal note for a rule whose journal reads', async () => {
    api.fetchTriggers.mockResolvedValue([view()])
    await mountList()
    expect(document.body.querySelector('[data-testid="trigger-journal-refused"]')).toBeNull()
  })

  it('names what stayed behind after a removal', async () => {
    api.fetchTriggers.mockResolvedValue([view()])
    api.removeTrigger.mockResolvedValue({
      removed: true,
      state_left: '.apb/trigger-state/project/state.json (it does not verify)',
      journal_left: '.apb/trigger-state/log/inc1.jsonl (a link at log)',
    })
    await mountList()
    const open = document.body.querySelector<HTMLButtonElement>('[data-testid="trigger-row"] button')
    open?.click()
    await vi.waitFor(() => {
      expect(document.body.querySelector('[data-testid="trigger-remove"]')).not.toBeNull()
    })
    document.body.querySelector<HTMLButtonElement>('[data-testid="trigger-remove"]')?.click()
    await vi.waitFor(() => {
      expect(document.body.querySelector('[data-testid="trigger-remove-ok"]')).not.toBeNull()
    })
    document.body.querySelector<HTMLButtonElement>('[data-testid="trigger-remove-ok"]')?.click()
    await vi.waitFor(() => {
      expect(toast.success).toHaveBeenCalled()
    })
    const call = toast.success.mock.calls.at(-1)
    const description = (call?.[1] as { description?: string } | undefined)?.description ?? ''
    expect(description).toContain('state_left')
    expect(description).toContain('.apb/trigger-state/project/state.json')
    expect(description).toContain('journal_left')
    expect(description).toContain('.apb/trigger-state/log/inc1.jsonl')
  })
})
