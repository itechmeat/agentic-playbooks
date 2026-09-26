import { describe, it, expect } from 'vitest'
import { provenanceLabel } from './versioninfo'
import type { VersionInfo } from './types'

describe('provenanceLabel', () => {
  it('labels a version with no sidecar as minor', () => {
    const v: VersionInfo = { version: '1.1.0', is_current: true, provenance: null }
    expect(provenanceLabel(v)).toBe('minor')
  })
  // Every save through apb writes a `created_by: user` sidecar; it is an
  // ordinary version, not a supervisor patch of unknown classification.
  it('labels a version saved by the user as minor, not as a patch', () => {
    const v: VersionInfo = {
      version: '1.14.0',
      is_current: false,
      provenance: { created_by: 'user', run_id: null, classification: null },
    }
    expect(provenanceLabel(v)).toBe('minor')
  })
  it('labels a supervisor patch with its classification and run', () => {
    const v: VersionInfo = {
      version: '1.0.1',
      is_current: false,
      provenance: { created_by: 'supervisor', run_id: 'run-1', classification: 'improvement' },
    }
    expect(provenanceLabel(v)).toBe('patch: improvement, run run-1')
  })
})
