import { describe, it, expect } from 'vitest'
import { toMcpToolsResult } from './connectors'

describe('toMcpToolsResult', () => {
  it('keeps the warnings of a failed listing', () => {
    expect(
      toMcpToolsResult({ ok: false, error: { code: 'transport', message: 'x' }, warnings: ['rotated'] }),
    ).toEqual({ ok: false, error: { code: 'transport', message: 'x' }, warnings: ['rotated'] })
  })

  it('reads a failure without warnings as none', () => {
    expect(toMcpToolsResult({ ok: false, error: { code: 'config', message: 'y' } })).toEqual({
      ok: false,
      error: { code: 'config', message: 'y' },
      warnings: [],
    })
  })
})
