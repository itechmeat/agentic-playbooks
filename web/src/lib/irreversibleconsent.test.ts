import { describe, it, expect } from 'vitest'
import { consentStep } from './irreversibleconsent'

describe('consentStep', () => {
  it('asks with the sources and the nonce of a first refusal', () => {
    expect(consentStep({ sources: ['node pr'], consent_nonce: 'consent-1' })).toEqual({
      kind: 'ask',
      sources: ['node pr'],
      nonce: 'consent-1',
    })
  })

  it('asks again when the tree changed and the refusal carries a new nonce', () => {
    const step = consentStep({ sources: ['node pr'], consent_nonce: 'consent-2' }, 'consent-1')
    expect(step).toMatchObject({ kind: 'ask', nonce: 'consent-2' })
  })

  // A refusal without a nonce used to reopen the dialog, whose confirm sent
  // no nonce and got the same refusal: an endless loop.
  it('stops with the refusal message when the refusal carries no nonce', () => {
    const step = consentStep({ sources: ['node pr'], detail: 'a trigger cannot consent' })
    expect(step).toEqual({ kind: 'stop', message: 'a trigger cannot consent' })
  })

  it('stops when the nonce it just sent is refused again', () => {
    const step = consentStep(
      { sources: ['node pr'], consent_nonce: 'consent-1', reason: 'refused again' },
      'consent-1',
    )
    expect(step).toEqual({ kind: 'stop', message: 'refused again' })
  })

  it('stops with a generic message when the refusal says nothing', () => {
    expect(consentStep(undefined).kind).toBe('stop')
  })
})
