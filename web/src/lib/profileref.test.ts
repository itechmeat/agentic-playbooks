import { describe, expect, it } from 'vitest'
import { fieldToProfile, profileToField } from './profileref'

describe('profileToField', () => {
  it('keeps a bare string (scope auto)', () => {
    expect(profileToField('arch')).toBe('arch')
  })
  it('encodes an explicit scope as scope/name', () => {
    expect(profileToField({ name: 'arch', scope: 'global' })).toBe('global/arch')
    expect(profileToField({ name: 'arch', scope: 'project' })).toBe('project/arch')
  })
  it('treats scope auto as bare name', () => {
    expect(profileToField({ name: 'arch', scope: 'auto' })).toBe('arch')
  })
  it('never yields [object Object]', () => {
    expect(profileToField({ name: 'x', scope: 'global' })).not.toContain('[object')
  })
  it('empty for missing/garbage', () => {
    expect(profileToField(undefined)).toBe('')
    expect(profileToField({})).toBe('')
  })
})

describe('fieldToProfile', () => {
  it('empty -> undefined', () => {
    expect(fieldToProfile('')).toBeUndefined()
    expect(fieldToProfile('   ')).toBeUndefined()
  })
  it('bare name -> string (scope auto)', () => {
    expect(fieldToProfile('arch')).toBe('arch')
  })
  it('scope/name -> typed object', () => {
    expect(fieldToProfile('global/arch')).toEqual({ name: 'arch', scope: 'global' })
    expect(fieldToProfile('project/arch')).toEqual({ name: 'arch', scope: 'project' })
  })
  it('unknown scope prefix is rejected (-> undefined)', () => {
    expect(fieldToProfile('weird/arch')).toBeUndefined()
  })
  it('rejects a nested/invalid name in a scoped ref', () => {
    // global/foo/bar: the name "foo/bar" is not a single valid segment -> reject.
    expect(fieldToProfile('global/foo/bar')).toBeUndefined()
  })
  it('rejects malformed bare names', () => {
    expect(fieldToProfile('Arch')).toBeUndefined() // uppercase
    expect(fieldToProfile('-arch')).toBeUndefined() // doesn't start with [a-z0-9]
    expect(fieldToProfile('a'.repeat(65))).toBeUndefined() // > 64
    expect(fieldToProfile('a b')).toBeUndefined() // space
  })
  it('accepts valid hyphenated names', () => {
    expect(fieldToProfile('a1-b2')).toBe('a1-b2')
    expect(fieldToProfile('project/a1-b2')).toEqual({ name: 'a1-b2', scope: 'project' })
  })
})

describe('round-trip', () => {
  it('preserves an explicit global ref (not degraded to scope auto)', () => {
    const ref = { name: 'arch', scope: 'global' }
    expect(fieldToProfile(profileToField(ref))).toEqual(ref)
  })
  it('distinguishes same-named project vs global', () => {
    const p = fieldToProfile(profileToField({ name: 'arch', scope: 'project' }))
    const g = fieldToProfile(profileToField({ name: 'arch', scope: 'global' }))
    expect(p).not.toEqual(g)
  })
  it('preserves a bare (scope auto) ref', () => {
    expect(fieldToProfile(profileToField('arch'))).toBe('arch')
  })
})
