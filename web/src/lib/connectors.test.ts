import { describe, expect, it } from 'vitest'
import { accountReady, trustBadge, type ConnectorAccount } from './connectors'

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
