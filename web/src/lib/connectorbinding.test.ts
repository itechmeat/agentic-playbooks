import { describe, expect, it } from 'vitest'
import { parseBinding, serializeBinding, toggleListEntry, type ConnectorBinding } from './connectorbinding'

describe('parseBinding', () => {
  it('bare string shorthand -> name with functions all, no accounts/max_calls', () => {
    expect(parseBinding('jira')).toEqual({ name: 'jira', functions: 'all' })
  })

  it('minimal object (name only) -> same as bare string (functions default all)', () => {
    expect(parseBinding({ name: 'jira' })).toEqual({ name: 'jira', functions: 'all' })
  })

  it('object with accounts allowlist', () => {
    expect(parseBinding({ name: 'telegram', accounts: ['team-bot'] })).toEqual({
      name: 'telegram',
      functions: 'all',
      accounts: ['team-bot'],
    })
  })

  it('functions: read_only string shorthand', () => {
    expect(parseBinding({ name: 'github', functions: 'read_only' })).toEqual({
      name: 'github',
      functions: 'read_only',
    })
  })

  it('functions as an explicit list', () => {
    expect(parseBinding({ name: 'telegram', functions: ['send_message'] })).toEqual({
      name: 'telegram',
      functions: ['send_message'],
    })
  })

  it('max_calls present', () => {
    expect(parseBinding({ name: 'telegram', max_calls: 50 })).toEqual({
      name: 'telegram',
      functions: 'all',
      maxCalls: 50,
    })
  })

  it('full object with every field', () => {
    expect(
      parseBinding({
        name: 'telegram',
        accounts: ['team-bot'],
        functions: ['send_message'],
        max_calls: 50,
      }),
    ).toEqual({
      name: 'telegram',
      accounts: ['team-bot'],
      functions: ['send_message'],
      maxCalls: 50,
    })
  })

  it('malformed shapes fall back to an empty name rather than throwing', () => {
    expect(parseBinding(42)).toEqual({ name: '', functions: 'all' })
    expect(parseBinding(null)).toEqual({ name: '', functions: 'all' })
    expect(parseBinding(['jira'])).toEqual({ name: '', functions: 'all' })
  })
})

describe('serializeBinding', () => {
  it('fully-default binding collapses to the bare string', () => {
    expect(serializeBinding({ name: 'jira' })).toBe('jira')
  })

  it('explicit functions: all also collapses to the bare string', () => {
    expect(serializeBinding({ name: 'jira', functions: 'all' })).toBe('jira')
  })

  it('accounts only -> object with name + accounts, no functions/max_calls keys', () => {
    expect(serializeBinding({ name: 'telegram', accounts: ['team-bot'] })).toEqual({
      name: 'telegram',
      accounts: ['team-bot'],
    })
  })

  it('functions: read_only -> object with the literal string form (not an expanded list)', () => {
    expect(serializeBinding({ name: 'github', functions: 'read_only' })).toEqual({
      name: 'github',
      functions: 'read_only',
    })
  })

  it('functions as an explicit list -> object with the list', () => {
    expect(serializeBinding({ name: 'telegram', functions: ['send_message'] })).toEqual({
      name: 'telegram',
      functions: ['send_message'],
    })
  })

  it('max_calls set -> object with name + max_calls', () => {
    expect(serializeBinding({ name: 'telegram', maxCalls: 50 })).toEqual({
      name: 'telegram',
      max_calls: 50,
    })
  })

  it('max_calls 0 is dropped (V26: a zero budget is invalid), collapses to bare string', () => {
    expect(serializeBinding({ name: 'jira', maxCalls: 0 })).toBe('jira')
  })

  it('negative or non-integer max_calls is dropped', () => {
    expect(serializeBinding({ name: 'jira', maxCalls: -1 })).toBe('jira')
    expect(serializeBinding({ name: 'jira', maxCalls: 1.5 })).toBe('jira')
  })

  it('every field set -> full object', () => {
    expect(
      serializeBinding({
        name: 'telegram',
        accounts: ['team-bot'],
        functions: ['send_message'],
        maxCalls: 50,
      }),
    ).toEqual({
      name: 'telegram',
      accounts: ['team-bot'],
      functions: ['send_message'],
      max_calls: 50,
    })
  })

  it('empty accounts list is preserved as an explicit deny-all, not collapsed', () => {
    expect(serializeBinding({ name: 'jira', accounts: [] })).toEqual({
      name: 'jira',
      accounts: [],
    })
  })
})

describe('roundtrip', () => {
  const cases: ConnectorBinding[] = [
    { name: 'jira', functions: 'all' },
    { name: 'telegram', accounts: ['team-bot'], functions: 'all' },
    { name: 'github', functions: 'read_only' },
    { name: 'telegram', functions: ['send_message'] },
    { name: 'telegram', accounts: ['team-bot'], functions: ['send_message'], maxCalls: 50 },
  ]

  for (const b of cases) {
    it(`round-trips ${JSON.stringify(b)}`, () => {
      expect(parseBinding(serializeBinding(b))).toEqual(b)
    })
  }

  it('a fully-default binding roundtrips through the bare-string shorthand', () => {
    const b: ConnectorBinding = { name: 'jira' }
    const yaml = serializeBinding(b)
    expect(yaml).toBe('jira')
    expect(parseBinding(yaml)).toEqual({ name: 'jira', functions: 'all' })
  })
})

describe('toggleListEntry', () => {
  const ALL = ['a', 'b', 'c']

  it('unchecking one entry from the absent (all-checked) state returns the rest', () => {
    expect(toggleListEntry(undefined, 'b', ALL)).toEqual(['a', 'c'])
  })

  it('rechecking the missing entry collapses back to the absent form', () => {
    const afterUncheck = toggleListEntry(undefined, 'b', ALL)
    expect(toggleListEntry(afterUncheck, 'b', ALL)).toBeUndefined()
  })

  it('collapse to undefined is order-independent (set equality, not array equality)', () => {
    expect(toggleListEntry(['c', 'a'], 'b', ALL)).toBeUndefined()
  })

  it('removing an entry present in an explicit list filters it out', () => {
    expect(toggleListEntry(['a', 'b'], 'a', ALL)).toEqual(['b'])
  })

  it('adding an entry not in an explicit (non-full) list appends it', () => {
    expect(toggleListEntry(['a'], 'b', ALL)).toEqual(['a', 'b'])
  })

  it('unchecking the last remaining entry yields an empty list, not undefined', () => {
    expect(toggleListEntry(['a'], 'a', ALL)).toEqual([])
  })
})
