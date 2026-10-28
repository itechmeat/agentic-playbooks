// @vitest-environment happy-dom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import NodePanel from './NodePanel.svelte'

let app: ReturnType<typeof mount> | null = null
beforeEach(() => {
  // The panel loads the playbook list, the connectors and the input draft;
  // none of that matters to the isolation fields.
  vi.stubGlobal('fetch', vi.fn(async () => new Response('[]', { status: 200 })))
})
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
})

function render(isolation: unknown, onChange: (patch: Record<string, unknown>) => void = () => {}) {
  const node = { id: 'w', type: 'agent_task', prompt: 'go', isolation }
  app = mount(NodePanel, { target: document.body, props: { id: 'p', node, onChange } })
  flushSync()
}

// Opens the select behind a trigger and picks the option with that label.
async function pick(testid: string, label: string) {
  const trigger = document.querySelector(`[data-testid="${testid}"]`) as HTMLElement
  trigger.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, button: 0, pointerType: 'mouse' }))
  await vi.waitFor(() => expect(document.querySelector('[role="option"]')).not.toBeNull())
  const option = [...document.querySelectorAll('[role="option"]')].find((o) => o.textContent?.trim() === label) as HTMLElement
  option.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, pointerType: 'mouse' }))
  option.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, button: 0, pointerType: 'mouse' }))
  option.click()
  flushSync()
}

const text = (testid: string) => document.querySelector(`[data-testid="${testid}"]`)?.textContent?.trim()

describe('NodePanel isolation', () => {
  it('shows the level, the network and the extra paths of the long map', () => {
    render({ level: 'full', network: 'none', read_only: ['~/.rustup'], writable: ['~/.cargo/registry'] })
    expect(text('np-isolation')).toBe('full')
    expect(text('np-isolation-network')).toBe('none')
    const paths = text('np-isolation-paths') ?? ''
    expect(paths).toContain('~/.rustup read-only')
    expect(paths).toContain('~/.cargo/registry writable')
    expect(document.body.textContent).not.toContain('[object Object]')
  })

  it('offers the network only for a level that isolates', () => {
    render('none')
    expect(text('np-isolation')).toBe('none')
    expect(document.querySelector('[data-testid="np-isolation-network"]')).toBeNull()
    expect(document.querySelector('[data-testid="np-isolation-paths"]')).toBeNull()
  })

  it('reads the shorthand with an implicit network', () => {
    render('best_effort')
    expect(text('np-isolation')).toBe('best_effort')
    expect(text('np-isolation-network')).toBe('(default: open)')
  })

  it('shows an unknown level or network as written, not as the default', () => {
    render({ level: 'fulll' })
    expect(text('np-isolation')).toBe('fulll (unknown)')
    unmount(app!)
    document.body.innerHTML = ''
    render({ level: 'full', network: 'half' })
    expect(text('np-isolation-network')).toBe('half (unknown)')
  })

  it('writes the chosen network into the long map', async () => {
    const patches: Record<string, unknown>[] = []
    render('full', (p) => patches.push(p))
    await pick('np-isolation-network', 'none')
    expect(patches).toEqual([{ isolation: { level: 'full', network: 'none' } }])
  })

  it('drops the paths and the network when the level falls to none', async () => {
    const patches: Record<string, unknown>[] = []
    render({ level: 'full', network: 'none', read_only: ['~/.rustup'], writable: ['~/.cargo'] }, (p) => patches.push(p))
    await pick('np-isolation', 'none')
    expect(patches).toEqual([{ isolation: 'none' }])
  })
})
