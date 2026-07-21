import { describe, expect, it } from 'vitest'
import { connectorHref, connectorRouteName, decodeSegment } from './route'

describe('decodeSegment', () => {
  it('decodes percent-encoding', () => {
    expect(decodeSegment('a%20b')).toBe('a b')
  })

  it('falls back to the raw segment on malformed encoding', () => {
    expect(decodeSegment('100%')).toBe('100%')
  })
})

describe('connectorRouteName', () => {
  it('reads the whole remainder as the name', () => {
    expect(connectorRouteName('github')).toBe('github')
  })

  it('resolves a legacy link with an empty workspace segment', () => {
    // `#/connector//github` was minted before connectors dropped the
    // workspace segment; it must still land on the same connector.
    expect(connectorRouteName('/github')).toBe('github')
  })

  it('decodes an encoded name', () => {
    expect(connectorRouteName('smtp%2Demail')).toBe('smtp-email')
  })

  it('ignores a trailing slash', () => {
    expect(connectorRouteName('github/')).toBe('github')
  })

  it('is empty for a bare route', () => {
    expect(connectorRouteName('')).toBe('')
    expect(connectorRouteName('/')).toBe('')
  })
})

describe('connectorHref', () => {
  it('builds a route with no empty workspace segment', () => {
    expect(connectorHref('github')).toBe('#/connector/github')
  })

  it('round-trips through the parser', () => {
    for (const name of ['github', 'smtp', 'my-connector']) {
      expect(connectorRouteName(connectorHref(name).slice('#/connector/'.length))).toBe(name)
    }
  })
})
