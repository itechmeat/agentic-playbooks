// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import DiagramText from './DiagramText.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('DiagramText', () => {
  it('shows the text collapsed under its summary', () => {
    app = mount(DiagramText, { target: document.body, props: { text: 'flowchart TD\n  n0 --> n1' } })
    flushSync()
    const block = document.querySelector<HTMLDetailsElement>('[data-testid="diagram-text"]')!
    expect(block.open).toBe(false)
    expect(block.querySelector('summary')?.textContent).toBe('Diagram text')
    expect(block.querySelector('pre')?.textContent).toBe('flowchart TD\n  n0 --> n1')
  })

  it('keeps the copy button inside the box that holds the text', () => {
    app = mount(DiagramText, { target: document.body, props: { text: 'flowchart TD' } })
    flushSync()
    const box = document.querySelector('[data-testid="diagram-box"]')!
    expect(box.querySelector('pre')).not.toBeNull()
    expect(box.querySelector('button')?.textContent?.trim()).toBe('copy')
  })
})
