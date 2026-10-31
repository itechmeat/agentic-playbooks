import { describe, expect, it } from 'vitest'
import type { Recall } from './api.gen'
import { instructionFrom, memoryNoteFrom, mergeRecalled, missingValues, paramFields, runInputFrom } from './runinput'
import type { PlaybookParam } from './types'

const params: PlaybookParam[] = [
  { name: 'channel', type: 'enum', options: ['beta', 'stable'], default: 'stable' },
  { name: 'count', type: 'int', label: 'How many' },
  { name: 'token', type: 'text', secret: true },
  { name: 'note', type: 'text', default: 'hello' },
]

const recall = (over: Partial<Recall> = {}): Recall => ({
  instruction: null,
  instruction_source: 'none',
  values: [],
  missing: [],
  schema_changed: false,
  diagnostics: [],
  ...over,
})

describe('paramFields', () => {
  it('prefills a compatible remembered value over the default', () => {
    const r = recall({ values: [{ name: 'channel', value: 'beta', status: { status: 'compatible' } }] })
    const f = paramFields(params, r)
    expect(f.find((x) => x.name === 'channel')?.value).toBe('beta')
    expect(f.find((x) => x.name === 'note')?.value).toBe('hello')
  })
  it('shows an incompatible value with its reason and never uses it', () => {
    const r = recall({
      schema_changed: true,
      values: [
        { name: 'count', value: 'many', status: { status: 'incompatible', reason: 'type_changed' } },
        { name: 'channel', value: 'gone', status: { status: 'incompatible', reason: 'option_gone' } },
      ],
      missing: ['count'],
    })
    const f = paramFields(params, r)
    const count = f.find((x) => x.name === 'count')!
    expect(count.value).toBe('')
    expect(count.note).toContain('no longer has the right type')
    expect(count.required).toBe(true)
    // With a default, the field shows the value the start will use.
    expect(f.find((x) => x.name === 'channel')?.value).toBe('stable')
  })
  it('never prefills a secret param, even with a value in memory', () => {
    const r = recall({ values: [{ name: 'token', value: 'x', status: { status: 'compatible' } }] })
    const token = paramFields(params, r).find((x) => x.name === 'token')!
    expect(token.secret).toBe(true)
    expect(token.value).toBe('')
  })
  it('falls back to the defaults and the param name as label without memory', () => {
    const f = paramFields(params, null)
    expect(f.map((x) => x.value)).toEqual(['stable', '', '', 'hello'])
    expect(f[1].label).toBe('How many')
    expect(missingValues(f)).toEqual(['count', 'token'])
  })
})

describe('mergeRecalled and memoryNoteFrom', () => {
  it('fills the untouched fields from memory and keeps what was typed', () => {
    const base = paramFields(params, null)
    const current = paramFields(params, null)
    current[1].value = '7'
    const r = recall({
      values: [
        { name: 'count', value: '4', status: { status: 'compatible' } },
        { name: 'note', value: 'hi', status: { status: 'compatible' } },
      ],
    })
    const merged = mergeRecalled(current, base, paramFields(params, r))
    expect(merged.map((f) => f.value)).toEqual(['stable', '7', '', 'hi'])
    expect(merged[1].remembered).toBe('4')
  })
  it('says what changed and what the server found', () => {
    expect(memoryNoteFrom(recall())).toBeNull()
    expect(memoryNoteFrom(recall({ schema_changed: true, diagnostics: ['moved aside'] }))).toBe(
      'The params changed since the input was remembered. moved aside',
    )
  })
})

describe('instruction and run body', () => {
  it('starts from the remembered instruction', () => {
    expect(instructionFrom(recall({ instruction: 'since the last tag', instruction_source: 'memory' }))).toBe(
      'since the last tag',
    )
    expect(instructionFrom(null)).toBe('')
  })
  it('sends the values the person chose, a non-blank instruction and the remember choice', () => {
    const f = paramFields(params, recall())
    f[1].value = '3'
    // Defaults left as they are stay with the engine, so remember never
    // stores a default nobody chose.
    expect(runInputFrom(f, '  ', false)).toEqual({ params: { count: '3' } })
    f[0].value = 'beta'
    expect(runInputFrom(f, 'go', true)).toEqual({
      params: { channel: 'beta', count: '3' },
      instruction: 'go',
      remember: true,
    })
  })
  it('sends a remembered value, and the default when the person cleared it', () => {
    const r = recall({
      values: [
        { name: 'channel', value: 'beta', status: { status: 'compatible' } },
        { name: 'note', value: 'hi', status: { status: 'compatible' } },
      ],
    })
    const f = paramFields(params, r)
    f.find((x) => x.name === 'note')!.value = ''
    f.find((x) => x.name === 'count')!.value = '1'
    expect(runInputFrom(f, '', false).params).toEqual({ channel: 'beta', count: '1', note: 'hello' })
    // A remembered value put back to the default is sent as such.
    f.find((x) => x.name === 'channel')!.value = 'stable'
    expect(runInputFrom(f, '', false).params?.channel).toBe('stable')
  })
  it('sends every shown value when memory could not be read', () => {
    // Left out, a value would be filled from memory the dialog never showed.
    const f = paramFields(params, null)
    f[1].value = '3'
    expect(runInputFrom(f, '', false).params).toEqual({ channel: 'stable', count: '3', note: 'hello' })
  })
  it('an empty default is a default: the field is not required', () => {
    const f = paramFields([{ name: 'tag', type: 'text', default: '' }], recall())
    expect(missingValues(f)).toEqual([])
    expect(runInputFrom(f, '', false).params).toEqual({})
    // Unknown memory: the empty default is sent as such.
    expect(runInputFrom(paramFields([{ name: 'tag', type: 'text', default: '' }], null), '', false).params).toEqual({
      tag: '',
    })
  })
  it('a cleared field without a default is missing, even when one was remembered', () => {
    const r = recall({ values: [{ name: 'count', value: '4', status: { status: 'compatible' } }] })
    const f = paramFields(params, r)
    expect(missingValues(f)).toEqual(['token'])
    f.find((x) => x.name === 'count')!.value = ''
    expect(missingValues(f)).toEqual(['count', 'token'])
  })
  it('sends an empty instruction when the person cleared the remembered one', () => {
    const f = paramFields([], null)
    expect(runInputFrom(f, '', false, 'since the last tag')).toEqual({ params: {}, instruction: '' })
    expect(runInputFrom(f, '', false, '')).toEqual({ params: {} })
  })
})
