import { describe, expect, it } from 'vitest'
import {
  accountReady,
  deprecationReason,
  isDeprecated,
  showAbout,
  showAccounts,
  showFunctions,
  showHeaderMeta,
  showPlayground,
  trustBadge,
  type ConnectorAccount,
  type ConnectorDetail,
  type ConnectorFunction,
} from './connectors'

describe('trustBadge', () => {
  it('approved -> ok tone', () => {
    expect(trustBadge('approved')).toEqual({ label: 'approved', tone: 'ok' })
  })
  it('changed -> warn tone', () => {
    expect(trustBadge('changed')).toEqual({ label: 'changed', tone: 'warn' })
  })
  it('unapproved -> muted tone', () => {
    expect(trustBadge('unapproved')).toEqual({ label: 'unapproved', tone: 'muted' })
  })
  it('invalid -> danger tone (unparsable connector)', () => {
    expect(trustBadge('invalid')).toEqual({ label: 'invalid', tone: 'danger' })
  })
  it('not_installed -> muted tone, never a warning', () => {
    // Not being connected is a state, not a trust problem, so it must not
    // borrow the warn or danger tone.
    expect(trustBadge('not_installed')).toEqual({ label: 'not connected', tone: 'muted' })
  })
})

describe('accountReady', () => {
  const base: ConnectorAccount = {
    name: 'default',
    default: true,
    fields: {},
    missingEnv: [],
    trust: 'approved',
  }

  it('ready when missingEnv is empty', () => {
    expect(accountReady(base)).toBe(true)
  })
  it('not ready when missingEnv has entries', () => {
    expect(accountReady({ ...base, missingEnv: ['API_KEY'] })).toBe(false)
  })
  it('readiness is independent of trust', () => {
    expect(accountReady({ ...base, trust: 'unapproved' })).toBe(true)
    expect(accountReady({ ...base, missingEnv: ['X'], trust: 'approved' })).toBe(false)
  })
})

const fn = (over: Partial<ConnectorFunction> = {}): ConnectorFunction => ({
  name: 'list_issues',
  description: 'List issues',
  readOnly: true,
  deprecated: null,
  argsSchema: null,
  ...over,
})

describe('deprecationReason', () => {
  it('null means not deprecated and renders no marker', () => {
    expect(deprecationReason(fn())).toBeNull()
    expect(isDeprecated(fn())).toBe(false)
  })
  it('an empty string does not read as deprecated', () => {
    expect(deprecationReason(fn({ deprecated: '' }))).toBeNull()
    expect(isDeprecated(fn({ deprecated: '' }))).toBe(false)
  })
  it('whitespace only does not read as deprecated either', () => {
    expect(deprecationReason(fn({ deprecated: '   ' }))).toBeNull()
    expect(isDeprecated(fn({ deprecated: '   ' }))).toBe(false)
  })
  it('a real reason is carried through, trimmed', () => {
    expect(deprecationReason(fn({ deprecated: ' use search_issues ' }))).toBe('use search_issues')
    expect(isDeprecated(fn({ deprecated: 'use search_issues' }))).toBe(true)
  })
})

const detail = (over: Partial<ConnectorDetail> = {}): ConnectorDetail => ({
  name: 'github',
  version: '1.0.0',
  installed: false,
  trust: 'not_installed',
  meta: {},
  bodyMd: '',
  functions: [],
  accounts: [],
  ...over,
})

const account: ConnectorAccount = {
  name: 'default',
  default: true,
  fields: {},
  missingEnv: [],
  trust: 'unapproved',
}

describe('section visibility', () => {
  it('About hides on an empty or whitespace-only body', () => {
    expect(showAbout(detail())).toBe(false)
    expect(showAbout(detail({ bodyMd: '  \n ' }))).toBe(false)
    expect(showAbout(detail({ bodyMd: '# GitHub' }))).toBe(true)
  })

  it('Functions hides when the connector declares none', () => {
    expect(showFunctions(detail())).toBe(false)
    expect(showFunctions(detail({ functions: [fn()] }))).toBe(true)
  })

  it('Accounts renders empty when installed: that empty state is the call to action', () => {
    expect(showAccounts(detail({ installed: true }))).toBe(true)
    expect(showAccounts(detail({ installed: true, accounts: [account] }))).toBe(true)
  })

  it('Accounts hides when not installed unless accounts are already configured', () => {
    expect(showAccounts(detail())).toBe(false)
    // Account config lives outside the installed files, so a not-installed
    // connector with configured accounts still shows the card.
    expect(showAccounts(detail({ accounts: [account] }))).toBe(true)
  })

  it('Playground needs both the installed files and a function to call', () => {
    expect(showPlayground(detail({ functions: [fn()] }))).toBe(false)
    expect(showPlayground(detail({ installed: true }))).toBe(false)
    expect(showPlayground(detail({ installed: true, functions: [fn()] }))).toBe(true)
  })

  it('the header meta row hides when there are no tags, publisher, or homepage', () => {
    expect(showHeaderMeta(detail())).toBe(false)
    expect(showHeaderMeta(detail({ meta: { tags: [] } }))).toBe(false)
    expect(showHeaderMeta(detail({ meta: { tags: ['git'] } }))).toBe(true)
    expect(showHeaderMeta(detail({ meta: { publisher: 'apb' } }))).toBe(true)
    expect(showHeaderMeta(detail({ meta: { homepage: 'https://example.com' } }))).toBe(true)
  })
})
