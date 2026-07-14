import { describe, it, expect } from 'vitest'
import { provenanceLabel } from './versioninfo'
import type { VersionInfo } from './types'

describe('provenanceLabel', () => {
  it('labels a user minor version with no provenance', () => {
    const v: VersionInfo = { version: '1.1.0', is_current: true, provenance: null }
    expect(provenanceLabel(v)).toBe('minor, current')
  })
  it('labels a promoted improvement patch from a run', () => {
    const v: VersionInfo = {
      version: '1.0.1',
      is_current: false,
      provenance: { created_by: 'supervisor', run_id: 'run-1', classification: 'improvement', promoted: true },
    }
    expect(provenanceLabel(v)).toBe('patch: improvement, promoted, run run-1')
  })
  it('labels an unpromoted workaround patch', () => {
    const v: VersionInfo = {
      version: '1.0.2',
      is_current: false,
      provenance: { created_by: 'supervisor', run_id: null, classification: 'workaround', promoted: false },
    }
    expect(provenanceLabel(v)).toBe('patch: workaround, not promoted')
  })
})
