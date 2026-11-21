import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import McpToolsSection from './McpToolsSection.svelte'
import type { McpAccountView, McpConnectorView } from '$lib/api/connectors'

// The server render of the section (`svelte/server`, as `Login.test.ts`
// does). The click of each row's "List tools" is in
// `McpToolsSection.svelte.test.ts`, which mounts the section in a DOM.

const account = (over: Partial<McpAccountView> = {}): McpAccountView => ({
  workspace: '',
  rootLabel: '',
  account: 'work',
  approvedAtMs: 1,
  functions: [{ function: 'whoami', tool: 'who_tool', approved: true, description: 'Who is signed in' }],
  ...over,
})

const view = (accounts: McpAccountView[]): McpConnectorView => ({
  transport: 'stdio',
  protocol: 'auto',
  command: '/usr/local/bin/tracker-mcp',
  url: null,
  auth: null,
  env: ['TRACKER_TOKEN'],
  accounts,
  pinsError: null,
})

describe('McpToolsSection', () => {
  it('shows the rows and no approve command', () => {
    const mcp = view([
      account({
        functions: [
          { function: 'whoami', tool: 'who_tool', approved: true, description: 'Who' },
          { function: 'create', tool: 'create_issue', approved: false, description: null },
        ],
      }),
    ])
    const { body } = render(McpToolsSection, { props: { connector: 'tracker-mcp', mcp, workspace: '' } })
    expect(body).toContain('MCP server')
    expect(body).toContain('stdio')
    expect(body).toContain('/usr/local/bin/tracker-mcp')
    expect(body).toContain('TRACKER_TOKEN')
    expect(body).toContain('whoami')
    expect(body).toContain('who_tool')
    expect(body).toContain('create_issue')
    expect(body).toContain('approved')
    expect(body).toContain('not approved')
    expect(body).toContain('List tools')
    expect(body).not.toContain('apb connector approve')
  })

  it('a description is rendered as text', () => {
    const mcp = view([
      account({
        functions: [{ function: 'whoami', tool: 'who_tool', approved: true, description: '<img src=x onerror=1>' }],
      }),
    ])
    const { body } = render(McpToolsSection, { props: { connector: 'tracker-mcp', mcp, workspace: '' } })
    expect(body).toContain('&lt;img')
    expect(body).not.toContain('<img')
  })

  it('each account row names its project', () => {
    const mcp = view([
      account({ workspace: 'ws-alpha', rootLabel: 'alpha' }),
      account({ workspace: 'ws-beta', rootLabel: 'beta', approvedAtMs: null }),
    ])
    const { body } = render(McpToolsSection, { props: { connector: 'tracker-mcp', mcp, workspace: '' } })
    expect(body).toContain('in alpha')
    expect(body).toContain('in beta')
  })
})
