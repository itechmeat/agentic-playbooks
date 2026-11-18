import { describe, it, expect } from 'vitest'
import { initialValues, invalidNumbers, toPayload } from './reviewform'
import type { FieldView } from './api.gen'

const amount: FieldView = { name: 'amount', kind: 'number', required: ['approve'], default: 1108.5 }
const currency: FieldView = { name: 'currency', kind: 'enum', default: 'EUR', options: ['EUR', 'USD'] }
const remark: FieldView = { name: 'remark', kind: 'text', max_len: 200 }
const sure: FieldView = { name: 'sure', kind: 'bool', default: false }

describe('reviewform', () => {
  it('initial values take the defaults', () => {
    expect(initialValues([amount, currency, remark, sure])).toEqual({
      amount: 1108.5,
      currency: 'EUR',
      remark: '',
      sure: false,
    })
  })

  it('a field the person cleared is sent blank for the server to decide', () => {
    // Whether a required value is missing is the server's one check: the
    // panel sends the blank, never the default it showed.
    expect(toPayload([amount, remark], { amount: '  ', remark: ' ' }, ['amount', 'remark'])).toEqual({
      amount: '',
      remark: ' ',
    })
    // Untouched blanks are no value: the server keeps the default.
    expect(toPayload([amount, remark], { amount: '', remark: '' })).toEqual({})
  })

  it('numbers and bools are sent typed', () => {
    const fields = [amount, sure, currency, remark]
    expect(
      toPayload(fields, { amount: '1180.5', sure: true, currency: 'USD', remark: 'ok' }),
    ).toEqual({ amount: 1180.5, sure: true, currency: 'USD', remark: 'ok' })
    // A value nobody gave is left out: the server keeps the default.
    expect(toPayload(fields, { amount: '', sure: false, currency: '', remark: '' })).toEqual({
      sure: false,
    })
  })

  it('a bool without a default starts unset and is posted only once chosen', () => {
    // CLI and MCP send nothing for a field nobody gave: the dashboard
    // never invents false, so an untouched bool is absent everywhere and a
    // required one is refused by the server's one check.
    const agreed: FieldView = { name: 'agreed', kind: 'bool', required: ['approve'] }
    const flag: FieldView = { name: 'flag', kind: 'bool' }
    expect(initialValues([agreed, flag, sure])).toEqual({ agreed: undefined, flag: undefined, sure: false })
    expect(toPayload([agreed, flag], initialValues([agreed, flag]))).toEqual({})
    // Chosen and then put back to "not chosen": no value again.
    expect(toPayload([agreed, flag], { agreed: undefined, flag: undefined }, ['agreed', 'flag'])).toEqual({})
    expect(toPayload([agreed, flag], { agreed: false, flag: true })).toEqual({ agreed: false, flag: true })
  })

  it('a cleared text field is sent empty, an untouched one is left out', () => {
    expect(toPayload([remark], { remark: '' }, ['remark'])).toEqual({ remark: '' })
    expect(toPayload([remark], { remark: '' })).toEqual({})
  })

  it('a number is read trimmed and text that is no number is named, never dropped', () => {
    // Only spaces: no value, never 0.
    expect(toPayload([amount], { amount: '   ' })).toEqual({})
    expect(toPayload([amount], { amount: ' 42 ' })).toEqual({ amount: 42 })
    expect(invalidNumbers([amount, remark], { amount: '12abc', remark: 'x' })).toEqual(['amount'])
    // Only a whole decimal number reads: no hex, no binary, no bare sign.
    for (const t of ['0x10', '0b11', '1_000', '-', '.', '1e', 'Infinity', '12 3']) {
      expect(invalidNumbers([amount], { amount: t })).toEqual(['amount'])
    }
    for (const t of ['12', ' -3.5 ', '+.5', '1e3', '2.']) {
      expect(invalidNumbers([amount], { amount: t })).toEqual([])
    }
    expect(invalidNumbers([amount], { amount: '' })).toEqual([])
    expect(invalidNumbers([amount], { amount: 7 })).toEqual([])
  })
})
