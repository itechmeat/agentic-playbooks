import { describe, expect, it } from 'vitest'
import {
  UNTRUSTED_NOTICE,
  formatReceived,
  inboxPanelState,
  previewBody,
  type ConnectorInbox,
} from './connectorinbox'

const inbox = (over: Partial<ConnectorInbox> = {}): ConnectorInbox => ({
  connector: 'echo-hooks',
  hasWebhook: true,
  publicBaseUrlSet: true,
  accounts: [
    {
      account: 'main',
      pending: 2,
      total: 5,
      cursor: 3,
      lastReceivedAt: 1_700_000_000_000,
      callbackUrl: 'https://hooks.example.com/hooks/echo-hooks/main',
      dropped: 0,
    },
  ],
  ...over,
})

describe('inboxPanelState', () => {
  it('hides the panel for a connector that cannot receive', () => {
    expect(inboxPanelState(true, false, inbox({ hasWebhook: false }))).toBe('hidden')
    expect(inboxPanelState(true, false, null)).toBe('hidden')
  })

  it('shows a loading state until the first answer arrives', () => {
    expect(inboxPanelState(false, false, null)).toBe('loading')
  })

  it('reports a failed request as its own state, never as an empty inbox', () => {
    expect(inboxPanelState(true, true, null)).toBe('error')
    expect(inboxPanelState(true, true, inbox())).toBe('error')
  })

  it('distinguishes a connector that can receive but has no account inbox yet', () => {
    expect(inboxPanelState(true, false, inbox({ accounts: [] }))).toBe('empty')
    expect(inboxPanelState(true, false, inbox())).toBe('ready')
  })
})

describe('formatReceived', () => {
  it('says so plainly when nothing has arrived', () => {
    expect(formatReceived(null)).toBe('never')
    expect(formatReceived(0)).toBe('never')
  })

  it('renders a timestamp as an ISO instant so it is unambiguous', () => {
    expect(formatReceived(1_700_000_000_000)).toBe('2023-11-14T22:13:20Z')
  })
})

describe('previewBody', () => {
  it('renders compact JSON', () => {
    expect(previewBody({ a: 1, b: 'x' }, 100)).toBe('{"a":1,"b":"x"}')
  })

  it('truncates a long body instead of flooding the page', () => {
    const long = { text: 'y'.repeat(500) }
    const out = previewBody(long, 40)
    expect(out.length).toBe(40)
    expect(out.endsWith('...')).toBe(true)
  })

  it('never throws on a value that cannot be serialized', () => {
    const cyclic: Record<string, unknown> = {}
    cyclic.self = cyclic
    expect(previewBody(cyclic, 40)).toBe('(unrenderable body)')
  })
})

describe('UNTRUSTED_NOTICE', () => {
  it('states the risk in plain words, with no exclamation mark or em-dash', () => {
    expect(UNTRUSTED_NOTICE).toContain('untrusted')
    expect(UNTRUSTED_NOTICE).not.toContain('!')
    expect(UNTRUSTED_NOTICE).not.toContain('—')
  })
})
