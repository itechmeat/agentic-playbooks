import { describe, expect, it } from 'vitest'
import {
  addTerm,
  addTermChecked,
  chosenName,
  fieldNames,
  fieldsText,
  searchErrorText,
  searchFooter,
  searchQuery,
  termRefusalText,
} from './runsearch'

describe('runsearch', () => {
  it('builds one field parameter per term after search=1', () => {
    expect(searchQuery([])).toBe('?search=1')
    expect(searchQuery([{ name: 'customer', value: 'Acme Inc' }], 'w1')).toBe(
      '?workspace=w1&search=1&field=customer%3AAcme+Inc',
    )
    const many = Array.from({ length: 10 }, (_, i) => ({ name: `p${i}`, value: 'v' }))
    expect(searchQuery(many).split('field=').length - 1).toBe(8)
  })

  it('offers the field names of the indexed runs only', () => {
    const runs = [
      { search: { state: 'not_indexed' as const } },
      {
        search: {
          state: 'indexed' as const,
          fields: [
            { name: 'customer', state: 'value' as const, value: 'Acme' },
            { name: 'ticket', state: 'withheld' as const },
          ],
        },
      },
      { search: { state: 'indexed' as const, fields: [{ name: 'customer', state: 'absent' as const }] } },
      {},
    ]
    expect(fieldNames(runs)).toEqual(['customer', 'ticket'])
    expect(fieldsText(runs[1].search)).toBe('customer Acme, ticket withheld')
    expect(fieldsText({ state: 'indexed', fields: [{ name: 'note', state: 'too_long' }] })).toBe('note too long')
    expect(fieldsText(undefined)).toBeNull()
  })

  it('says how many runs could not be searched', () => {
    expect(searchFooter({ not_indexed: 0, withheld: 0 }, true)).toBeNull()
    expect(searchFooter({ not_indexed: 1, withheld: 0 }, false)).toBe('1 run has no search fields')
    expect(searchFooter({ not_indexed: 2, withheld: 1 }, true)).toBe(
      '2 runs have no search fields; 1 run left out: a searched value is withheld',
    )
  })

  it('replaces a term of the same name and refuses an empty one', () => {
    let t = addTerm([], { name: 'customer', value: ' Acme ' })
    expect(t).toEqual([{ name: 'customer', value: 'Acme' }])
    t = addTerm(t, { name: 'customer', value: 'Globex' })
    expect(t).toEqual([{ name: 'customer', value: 'Globex' }])
    expect(addTerm(t, { name: 'ticket', value: '  ' })).toBe(t)
  })

  it('refuses a value over the 200 character cap', () => {
    const t = addTerm([], { name: 'customer', value: 'Acme' })
    const long = 'x'.repeat(201)
    expect(addTerm(t, { name: 'ticket', value: long })).toBe(t)
    expect(addTermChecked(t, { name: 'ticket', value: long }).refused).toBe('too_long')
    expect(addTermChecked(t, { name: 'ticket', value: 'x'.repeat(200) }).refused).toBeNull()
    expect(addTermChecked(t, { name: 'ticket', value: '   ' }).refused).toBe('empty')
    const full = Array.from({ length: 8 }, (_, i) => ({ name: `p${i}`, value: 'v' }))
    expect(addTermChecked(full, { name: 'q', value: 'v' }).refused).toBe('limit')
  })

  it('says why a term was refused, in plain words', () => {
    expect(termRefusalText('empty')).toContain('value')
    expect(termRefusalText('limit')).toContain('8')
    expect(termRefusalText('too_long')).toContain('200')
  })

  it('keeps the chosen field to a name the runs offer', () => {
    expect(chosenName(['a', 'b'], '')).toBe('a')
    expect(chosenName(['a', 'b'], 'b')).toBe('b')
    expect(chosenName(['a', 'b'], 'gone')).toBe('a')
    expect(chosenName([], 'a')).toBe('')
  })

  it('surfaces the server reason for a refused search, not the raw query', () => {
    expect(searchErrorText({ detail: 'value too long' })).toBe('value too long')
    expect(searchErrorText(new Error('/api/runs?search=1&field=a%3Ab: HTTP 400'))).toBe('HTTP 400')
    // `requestJson` names the response's absolute URL: its origin and its
    // query (the search terms) are dropped too.
    expect(
      searchErrorText(new Error('http://127.0.0.1:7878/api/runs?search=1&field=customer%3AAcme: workspace w1: boom')),
    ).toBe('workspace w1: boom')
    expect(searchErrorText('boom')).toBe('boom')
  })
})
