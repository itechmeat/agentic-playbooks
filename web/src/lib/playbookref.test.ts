import { describe, expect, it } from 'vitest'
import { fieldToPlaybookRef, playbookRefToField } from './playbookref'

describe('playbookRefToField', () => {
  it('keeps a bare string (scope auto)', () => {
    expect(playbookRefToField('child')).toBe('child')
  })
  it('encodes an explicit scope as scope/id', () => {
    expect(playbookRefToField({ id: 'child', scope: 'global' })).toBe('global/child')
    expect(playbookRefToField({ id: 'child', scope: 'project' })).toBe('project/child')
  })
  it('treats scope auto as bare id', () => {
    expect(playbookRefToField({ id: 'child', scope: 'auto' })).toBe('child')
  })
  it('never yields [object Object]', () => {
    expect(playbookRefToField({ id: 'x', scope: 'global' })).not.toContain('[object')
  })
  it('empty for missing/garbage', () => {
    expect(playbookRefToField(undefined)).toBe('')
    expect(playbookRefToField({})).toBe('')
  })
})

describe('fieldToPlaybookRef', () => {
  it('empty -> undefined', () => {
    expect(fieldToPlaybookRef('')).toBeUndefined()
    expect(fieldToPlaybookRef('   ')).toBeUndefined()
  })
  it('bare id -> string (scope auto)', () => {
    expect(fieldToPlaybookRef('child')).toBe('child')
  })
  it('scope/id -> typed object', () => {
    expect(fieldToPlaybookRef('global/child')).toEqual({ id: 'child', scope: 'global' })
    expect(fieldToPlaybookRef('project/child')).toEqual({ id: 'child', scope: 'project' })
  })
  it('unknown scope prefix is rejected (-> undefined)', () => {
    expect(fieldToPlaybookRef('weird/child')).toBeUndefined()
  })
  it('rejects a nested/invalid id in a scoped ref', () => {
    // global/foo/bar: the id "foo/bar" is not a single valid segment -> reject.
    expect(fieldToPlaybookRef('global/foo/bar')).toBeUndefined()
  })
  it('accepts ids outside the profile-name character class', () => {
    // Unlike profile names, playbook ids are only constrained by
    // is_safe_segment (no path separators, no ..), not [a-z0-9-].
    expect(fieldToPlaybookRef('My_Playbook.v2')).toBe('My_Playbook.v2')
  })
})

describe('round-trip', () => {
  it('preserves an explicit global ref (not degraded to scope auto)', () => {
    const ref = { id: 'child', scope: 'global' as const }
    expect(fieldToPlaybookRef(playbookRefToField(ref))).toEqual(ref)
  })
  it('distinguishes same-id project vs global', () => {
    const p = fieldToPlaybookRef(playbookRefToField({ id: 'child', scope: 'project' }))
    const g = fieldToPlaybookRef(playbookRefToField({ id: 'child', scope: 'global' }))
    expect(p).not.toEqual(g)
  })
  it('preserves a bare (scope auto) ref', () => {
    expect(fieldToPlaybookRef(playbookRefToField('child'))).toBe('child')
  })
})
