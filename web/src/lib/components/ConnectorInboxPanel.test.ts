import { describe, expect, it } from 'vitest'
import { render } from 'svelte/server'
import ConnectorInboxPanel from './ConnectorInboxPanel.svelte'
import type { ConnectorInbox } from '../connectorinbox'

const inbox: ConnectorInbox = {
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
    },
  ],
}

describe('ConnectorInboxPanel', () => {
  it('renders the depth, the last received instant and the callback URL', () => {
    const { body } = render(ConnectorInboxPanel, {
      props: { name: 'echo-hooks', inbox, loaded: true, failed: false },
    })
    expect(body).toContain('Inbox')
    expect(body).toContain('main')
    expect(body).toContain('2')
    expect(body).toContain('2023-11-14T22:13:20Z')
    expect(body).toContain('https://hooks.example.com/hooks/echo-hooks/main')
  })

  it('warns instead of a URL when no public base is configured', () => {
    const { body } = render(ConnectorInboxPanel, {
      props: {
        name: 'echo-hooks',
        inbox: {
          ...inbox,
          publicBaseUrlSet: false,
          accounts: [{ ...inbox.accounts[0], callbackUrl: null }],
        },
        loaded: true,
        failed: false,
      },
    })
    expect(body).toContain('public_base_url')
    expect(body).not.toContain('https://hooks.example.com')
  })

  it('marks event content as untrusted wherever it can be expanded', () => {
    const { body } = render(ConnectorInboxPanel, {
      props: {
        name: 'echo-hooks',
        inbox,
        loaded: true,
        failed: false,
        events: [],
        eventsAccount: 'main',
      },
    })
    expect(body).toContain('untrusted')
  })

  it('keeps every event body collapsed until that event is expanded', () => {
    const events = [
      { seq: 1, receivedAt: 1_700_000_000_000, body: { text: 'first-secret' } },
      { seq: 2, receivedAt: 1_700_000_000_001, body: { text: 'second-secret' } },
    ]
    const collapsed = render(ConnectorInboxPanel, {
      props: {
        name: 'echo-hooks',
        inbox,
        loaded: true,
        failed: false,
        events,
        eventsAccount: 'main',
      },
    }).body
    expect(collapsed).toContain('#1')
    expect(collapsed).toContain('Show body')
    expect(collapsed).not.toContain('first-secret')
    expect(collapsed).not.toContain('second-secret')

    // Expanding one event reveals that one and only that one.
    const opened = render(ConnectorInboxPanel, {
      props: {
        name: 'echo-hooks',
        inbox,
        loaded: true,
        failed: false,
        events,
        eventsAccount: 'main',
        expandedSeqs: [1],
      },
    }).body
    expect(opened).toContain('first-secret')
    expect(opened).not.toContain('second-secret')
  })

  it('renders nothing for a connector that cannot receive', () => {
    const { body } = render(ConnectorInboxPanel, {
      props: {
        name: 'plain',
        inbox: { ...inbox, hasWebhook: false, accounts: [] },
        loaded: true,
        failed: false,
      },
    })
    // Svelte 5's server renderer always emits its own hydration boundary
    // comments around a component's output, even an empty one, so "renders
    // nothing" is checked past those rather than against a literal empty
    // string (deviation from task-12-brief.md: the brief's own reference
    // assertion is unsatisfiable against this Svelte version).
    expect(body.replace(/<!--.*?-->/gs, '').trim()).toBe('')
  })

  it('escapes hostile text coming from an event body', () => {
    const hostile = { text: '<img src=x onerror=alert(1)>' }
    const { body } = render(ConnectorInboxPanel, {
      props: {
        name: 'echo-hooks',
        inbox,
        loaded: true,
        failed: false,
        events: [{ seq: 1, receivedAt: 1_700_000_000_000, body: hostile }],
        eventsAccount: 'main',
        expandedSeqs: [1],
      },
    })
    expect(body).not.toContain('<img')
    expect(body).toContain('&lt;img')
  })
})
