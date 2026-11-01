// @vitest-environment happy-dom
// The page reloads under an open "Make current" dialog on every change
// event: the move still expects the `current` the person was shown, so a
// move made elsewhere meanwhile comes back stale instead of being undone
// unseen. Runes need the `.svelte.` infix.
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import MakeCurrentDialog from './MakeCurrentDialog.svelte'
import type { MoveOutcome } from './currentmove'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)

async function settle() {
  for (let i = 0; i < 5; i++) await tick()
  flushSync()
}

describe('MakeCurrentDialog under a reloading page', () => {
  it('posts the current it showed, and after a stale answer the one the server named', async () => {
    const answers: MoveOutcome[] = [{ kind: 'stale', actual: '1.2.0' }]
    const submit = vi.fn(async (): Promise<MoveOutcome> => answers.shift() ?? { kind: 'refused', message: 'x' })
    const props = $state({
      open: true,
      playbookId: 'demo',
      from: '1.0.0' as string | null,
      to: '1.1.0',
      submit,
      // A reload that did not land: the page never shows 1.2.0.
      onstale: async () => {},
    })
    app = mount(MakeCurrentDialog, { target: document.body, props })
    flushSync()
    // A move elsewhere reloads the page: the dialog still shows, and
    // expects, what the person saw.
    props.from = '1.3.0'
    flushSync()
    expect(byTestId('make-current-move')?.textContent).toContain('1.0.0')
    byTestId('make-current-confirm')?.click()
    await settle()
    expect(submit).toHaveBeenLastCalledWith('', '1.0.0')
    // Told it is stale, the dialog moves from what the server said
    // `current` holds now, whatever the page shows.
    expect(byTestId('make-current-move')?.textContent).toContain('1.2.0')
    byTestId('make-current-confirm')?.click()
    await settle()
    expect(submit).toHaveBeenLastCalledWith('', '1.2.0')
  })
})
