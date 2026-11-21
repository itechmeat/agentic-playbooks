// @vitest-environment happy-dom
import { describe, it, expect, afterEach, vi } from 'vitest'
import { flushSync, mount, unmount, tick } from 'svelte'
import type { McpAccountView, McpConnectorView } from '$lib/api/connectors'

// Each account row's "List tools" lists in the project that row names,
// never the page's: the mounted section, its API call replaced.

const listMcpTools = vi.fn()
vi.mock('$lib/api/connectors', () => ({ listMcpTools: (...a: unknown[]) => listMcpTools(...a) }))

const { default: McpToolsSection } = await import('./McpToolsSection.svelte')
const { auth } = await import('$lib/auth.svelte')
const { ApiError } = await import('$lib/api/http')
const { LISTING_REFUSED } = await import('$lib/mcpTools')

const account = (workspace: string, rootLabel: string): McpAccountView => ({
  workspace,
  rootLabel,
  account: 'work',
  approvedAtMs: null,
  functions: [{ function: 'whoami', tool: 'who_tool', approved: true, description: 'Who' }],
})

const mcp: McpConnectorView = {
  transport: 'stdio',
  protocol: 'auto',
  command: '/usr/local/bin/tracker-mcp',
  url: null,
  auth: null,
  env: [],
  accounts: [account('ws-alpha', 'alpha'), account('ws-beta', 'beta')],
  pinsError: null,
}

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  listMcpTools.mockReset()
  auth.set({ required: false, authenticated: true, checked: false })
})

const mountSection = () => {
  app = mount(McpToolsSection, {
    target: document.body,
    props: { connector: 'tracker-mcp', mcp, workspace: 'ws-page' },
  })
  flushSync()
}

const listButtons = () =>
  [...document.querySelectorAll('button')].filter((b) => b.textContent?.includes('List tools'))

const settle = async () => {
  await tick()
  await new Promise((r) => setTimeout(r, 0))
  flushSync()
}

describe('McpToolsSection (mounted)', () => {
  it('each account row names its project', async () => {
    listMcpTools.mockResolvedValue({
      ok: false,
      error: { code: 'permission', message: 'mcp_server_changed: the account changed' },
      warnings: [],
    })
    app = mount(McpToolsSection, {
      target: document.body,
      props: { connector: 'tracker-mcp', mcp, workspace: 'ws-page' },
    })
    flushSync()
    const buttons = [...document.querySelectorAll('button')].filter((b) => b.textContent?.includes('List tools'))
    expect(buttons.length).toBe(2)
    buttons[1].click()
    buttons[0].click()
    await tick()
    expect(listMcpTools.mock.calls).toEqual([
      ['tracker-mcp', 'work', 'ws-beta'],
      ['tracker-mcp', 'work', 'ws-alpha'],
    ])
    await new Promise((r) => setTimeout(r, 0))
    flushSync()
    expect(document.body.textContent).toContain('mcp_server_changed: the account changed')
  })

  it('names each row button and result for its account and project', () => {
    mountSection()
    expect(listButtons().map((b) => b.getAttribute('aria-label'))).toEqual([
      'List tools of work in alpha',
      'List tools of work in beta',
    ])
    const regions = [...document.querySelectorAll('[aria-live]')]
    expect(regions.map((r) => r.getAttribute('aria-live'))).toEqual(['polite', 'polite'])
    expect(regions.map((r) => r.getAttribute('aria-label'))).toEqual([
      'Live listing of work in alpha',
      'Live listing of work in beta',
    ])
  })

  it('the result region is busy while the row lists', async () => {
    let resolve: (v: unknown) => void = () => {}
    listMcpTools.mockReturnValue(new Promise((r) => (resolve = r)))
    mountSection()
    listButtons()[0].click()
    flushSync()
    const region = document.querySelectorAll('[aria-live]')[0]
    expect(region.getAttribute('aria-busy')).toBe('true')
    resolve({ ok: false, error: { code: 'config', message: 'boom' }, warnings: [] })
    await settle()
    expect(region.getAttribute('aria-busy')).toBe('false')
    expect(region.textContent).toContain('boom')
  })

  it('a failed listing shows its warnings next to the error', async () => {
    listMcpTools.mockResolvedValue({
      ok: false,
      error: { code: 'transport', message: 'discovery failed' },
      warnings: ['the token endpoint returned a new refresh token'],
    })
    mountSection()
    listButtons()[0].click()
    await settle()
    const region = document.querySelectorAll('[aria-live]')[0]
    expect(region.textContent).toContain('discovery failed')
    expect(region.textContent).toContain('the token endpoint returned a new refresh token')
  })

  it('a 403 shows the refused state with its fixed words, not the server text', async () => {
    listMcpTools.mockRejectedValue(new ApiError('mcp_needs_a_person: SERVER-TEXT-17', 403))
    mountSection()
    listButtons()[0].click()
    await settle()
    const region = document.querySelectorAll('[aria-live]')[0]
    expect(region.textContent).toContain(LISTING_REFUSED)
    expect(document.body.textContent).not.toContain('SERVER-TEXT-17')
  })

  it('another failure shows its own message', async () => {
    listMcpTools.mockRejectedValue(new ApiError('network down', 500))
    mountSection()
    listButtons()[0].click()
    await settle()
    expect(document.body.textContent).toContain('network down')
    expect(document.body.textContent).not.toContain(LISTING_REFUSED)
  })

  it('a dashboard without keys offers no button and says why', () => {
    auth.set({ required: false, authenticated: true, checked: true })
    mountSection()
    expect(listButtons()).toEqual([])
    expect(document.body.textContent).toContain(LISTING_REFUSED)
  })

  it('a signed-in session is offered the button', () => {
    auth.set({ required: true, authenticated: true, checked: true })
    mountSection()
    expect(listButtons().length).toBe(2)
    expect(document.body.textContent).not.toContain(LISTING_REFUSED)
  })
})
