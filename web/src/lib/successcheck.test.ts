import { describe, expect, it } from 'vitest'
import {
  fieldsToSuccessCheck,
  successCheckMode,
  successCheckToFields,
} from './successcheck'

describe('successCheckToFields', () => {
  it('maps a bare script string to script mode', () => {
    expect(successCheckToFields('scripts/verify.sh')).toEqual({
      mode: 'script',
      value: 'scripts/verify.sh',
    })
  })

  it('maps a marker object without coercing it to a string', () => {
    expect(successCheckToFields({ marker: 'WAVE-COMPLETE' })).toEqual({
      mode: 'marker',
      value: 'WAVE-COMPLETE',
    })
  })

  it('never yields [object Object] for a marker form', () => {
    expect(successCheckToFields({ marker: 'DONE' }).value).not.toContain('[object')
  })

  it('is empty script mode for missing/garbage', () => {
    expect(successCheckToFields(undefined)).toEqual({ mode: 'script', value: '' })
    expect(successCheckToFields(null)).toEqual({ mode: 'script', value: '' })
    expect(successCheckToFields({})).toEqual({ mode: 'script', value: '' })
  })
})

describe('fieldsToSuccessCheck', () => {
  it('empty -> undefined for either mode', () => {
    expect(fieldsToSuccessCheck('script', '')).toBeUndefined()
    expect(fieldsToSuccessCheck('script', '   ')).toBeUndefined()
    expect(fieldsToSuccessCheck('marker', '')).toBeUndefined()
  })

  it('script mode yields a bare string', () => {
    expect(fieldsToSuccessCheck('script', 'scripts/check.sh')).toBe('scripts/check.sh')
  })

  it('marker mode yields { marker }', () => {
    expect(fieldsToSuccessCheck('marker', 'WAVE-COMPLETE')).toEqual({
      marker: 'WAVE-COMPLETE',
    })
  })

  it('trims whitespace on both forms', () => {
    expect(fieldsToSuccessCheck('script', '  scripts/a.sh  ')).toBe('scripts/a.sh')
    expect(fieldsToSuccessCheck('marker', '  DONE  ')).toEqual({ marker: 'DONE' })
  })
})

describe('successCheckMode', () => {
  it('detects marker vs script from a stored value', () => {
    expect(successCheckMode('scripts/a.sh')).toBe('script')
    expect(successCheckMode({ marker: 'X' })).toBe('marker')
    expect(successCheckMode(undefined)).toBe('script')
  })
})

describe('round-trip', () => {
  it('preserves a script-form success_check', () => {
    const raw = 'scripts/verify.sh'
    const fields = successCheckToFields(raw)
    expect(fieldsToSuccessCheck(fields.mode, fields.value)).toBe(raw)
  })

  it('preserves a marker-form success_check (not degraded to a string)', () => {
    const raw = { marker: 'WAVE-COMPLETE' }
    const fields = successCheckToFields(raw)
    expect(fieldsToSuccessCheck(fields.mode, fields.value)).toEqual(raw)
  })
})
