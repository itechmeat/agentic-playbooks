import { describe, expect, it } from 'vitest'
import { fieldsToField, fieldToOutputs, sessionSources } from './nodeio'

describe('fieldsToField', () => {
  it('lists declared fields comma separated', () => {
    expect(fieldsToField({ fields: ['working_tree', 'verdict'] })).toBe('working_tree, verdict')
  })
  it('is empty without a declaration', () => {
    expect(fieldsToField(undefined)).toBe('')
    expect(fieldsToField({ files: ['a.md'] })).toBe('')
  })
})

describe('fieldsToField with typed entries', () => {
  it('shows each entry by its name, never as an object', () => {
    expect(
      fieldsToField({ fields: [{ name: 'blocking', type: 'number', required: true }, 'notes'] }),
    ).toBe('blocking, notes')
  })
})

describe('fieldToOutputs', () => {
  it('keeps a typed entry whose name is still listed, drops a removed one, adds new names plain', () => {
    const typed = { name: 'blocking', type: 'number', required: true }
    const other = { name: 'old', type: 'string' }
    expect(fieldToOutputs('blocking, notes', { fields: [typed, other] })).toEqual({
      fields: [typed, 'notes'],
    })
  })

  it('sets the fields and keeps the other outputs keys', () => {
    expect(fieldToOutputs('working_tree,  verdict ,', { files: ['a.md'], extract: 'X' })).toEqual({
      files: ['a.md'],
      extract: 'X',
      fields: ['working_tree', 'verdict'],
    })
  })
  it('drops the key, and outputs itself when nothing else is left', () => {
    expect(fieldToOutputs('', { files: ['a.md'], fields: ['x'] })).toEqual({ files: ['a.md'] })
    expect(fieldToOutputs(' ', { fields: ['x'] })).toBeUndefined()
    expect(fieldToOutputs('', undefined)).toBeUndefined()
  })
})

describe('sessionSources', () => {
  it('offers the other agent_task nodes', () => {
    const nodes = [
      { id: 'start', type: 'start' },
      { id: 'assess', type: 'agent_task' },
      { id: 'implement', type: 'agent_task' },
      { id: 'gate', type: 'script' },
    ]
    expect(sessionSources(nodes, 'implement')).toEqual(['assess'])
  })
})
