// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import IrreversibleRunDialog from './IrreversibleRunDialog.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)

describe('IrreversibleRunDialog', () => {
  // The dialog lists every structured source, including one whose node id
  // carries a `;`, which the old `detail.split(';')` rendering cut short.
  it('lists every source of the refusal, and only its confirm button consents', () => {
    const onconfirm = vi.fn()
    const sources = ['node pr', 'node a;b', 'sub-playbook node ship']
    app = mount(IrreversibleRunDialog, {
      target: document.body,
      props: { open: true, playbookId: 'p', sources, onconfirm },
    })
    flushSync()
    const items = [...document.querySelectorAll('[data-testid="irreversible-sources"] li')]
    expect(items.map((li) => li.textContent?.trim())).toEqual(sources)
    expect(byTestId('irreversible-dialog')?.textContent).toContain('p')

    byTestId('irreversible-cancel')?.click()
    flushSync()
    expect(onconfirm).not.toHaveBeenCalled()
  })

  it('calls onconfirm once when the person confirms', () => {
    const onconfirm = vi.fn()
    app = mount(IrreversibleRunDialog, {
      target: document.body,
      props: { open: true, playbookId: 'p', sources: ['node pr'], onconfirm },
    })
    flushSync()
    byTestId('irreversible-confirm')?.click()
    flushSync()
    expect(onconfirm).toHaveBeenCalledTimes(1)
  })
})
