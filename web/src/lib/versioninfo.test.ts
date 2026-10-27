import { describe, it, expect } from 'vitest'
import { approvalBadge, approvalHint, candidateTrialLabel, provenanceLabel, trialBadge } from './versioninfo'
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

describe('forward patches and candidate trials', () => {
  const forward = (extra: Partial<VersionInfo>): VersionInfo => ({
    version: '1.0.1',
    is_current: false,
    provenance: {
      created_by: 'supervisor',
      run_id: 'run-1',
      classification: 'improvement',
      scope: 'next_runs',
    },
    ...extra,
  })
  it('labels a forward patch as such', () => {
    expect(provenanceLabel(forward({}))).toBe('forward patch: improvement, run run-1')
  })
  it('badges the candidate, then its outcome', () => {
    expect(trialBadge(forward({ is_candidate: true }))).toBe('candidate')
    const promoted = forward({})
    promoted.provenance!.trial = { successes: 1, outcome: 'promoted', run_id: 'run-2' }
    expect(trialBadge(promoted)).toBe('promoted')
    expect(trialBadge({ version: '1.0.0', is_current: true, provenance: null })).toBeNull()
  })
  it('labels a candidate trial run', () => {
    expect(candidateTrialLabel({ version: '1.0.1', verdict: 'rejected' })).toBe(
      'candidate 1.0.1: rejected',
    )
    expect(candidateTrialLabel(undefined)).toBeNull()
  })
})

describe('imported versions', () => {
  const imported = (file_name?: string): VersionInfo => ({
    version: '2.0.0',
    is_current: true,
    provenance: {
      created_by: 'import',
      run_id: null,
      classification: null,
      imported: {
        bundle_digest: 'sha256:b',
        file_name,
        source_id: 'p',
        source_version: '2.0.0',
        imported_at_ms: 1,
        digest: 'sha256:d',
      },
    },
  })
  it('names the bundle file it came from', () => {
    expect(provenanceLabel(imported('release.apb.json'))).toBe('imported from release.apb.json')
  })
  it('says imported when the bundle had no file name', () => {
    expect(provenanceLabel(imported())).toBe('imported')
  })
  it('badges a version that waits for an approval by digest, and no other', () => {
    expect(approvalBadge({ ...imported(), awaiting_approval: true })).toBe('awaiting approval')
    expect(approvalBadge(imported())).toBeNull()
  })
  it('labels a save that kept the scripts of an unapproved import', () => {
    const saved = imported('release.apb.json')
    saved.provenance!.created_by = 'user'
    expect(provenanceLabel(saved)).toBe('minor, with imported scripts')
  })
  it('hints the approval without a digest that may be stale', () => {
    const hint = approvalHint('p', '2.0.0')
    expect(hint).toContain('apb trust approve p --version 2.0.0')
    expect(hint).not.toContain('sha256:')
  })
})
