// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import Fixture from './select-content.fixture.svelte'

// Regression: picking a project in the new-playbook editor and clicking Save
// right away did nothing, and the second click saved. An open select locked
// the page with `pointer-events: none` on <body> and released it only after
// its close animation, so a click inside that window landed on <html> and was
// lost. A select must never take the page's clicks away.

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  document.body.removeAttribute('style')
})

async function settle() {
  for (let i = 0; i < 5; i++) {
    await tick()
    await new Promise((r) => setTimeout(r, 0))
  }
}

describe('Select content', () => {
  it('keeps the page clickable right after an option is picked', async () => {
    app = mount(Fixture, { target: document.body, props: { open: true } })
    await settle()
    const option = [...document.querySelectorAll<HTMLElement>('[data-slot="select-item"]')].find(
      (el) => el.textContent?.trim() === 'b',
    )
    expect(option).toBeTruthy()
    // bits-ui picks on pointerup, like a real mouse release over the option.
    option!.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerType: 'mouse' }))
    flushSync()

    expect(document.querySelector('[data-slot="select-trigger"]')?.textContent?.trim()).toBe('b')
    expect(getComputedStyle(document.body).pointerEvents).not.toBe('none')
  })
})
