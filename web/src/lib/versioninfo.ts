import type { CandidateTrial } from './api.gen'
import type { VersionInfo } from './types'

// Human-readable origin of a version for the history panel. Which version is
// in use is not part of it: the `current` badge shows that, from `is_current`,
// the one source for it (the old stored `promoted` flag drifted from it).
export function provenanceLabel(v: VersionInfo): string {
  const p = v.provenance
  if (p?.created_by !== 'supervisor') return 'minor'
  const kind = p.scope === 'next_runs' ? 'forward patch' : 'patch'
  const parts = [`${kind}: ${p.classification ?? 'unknown'}`]
  if (p.run_id) parts.push(`run ${p.run_id}`)
  return parts.join(', ')
}

// The trial state of a forward patch (issue #192) for the history badge:
// `candidate` while the pointer names it, else its recorded outcome.
export function trialBadge(v: VersionInfo): string | null {
  if (v.is_candidate) return 'candidate'
  return v.provenance?.trial?.outcome ?? null
}

// One line for a run that was a candidate trial, null for any other run.
export function candidateTrialLabel(t: CandidateTrial | undefined): string | null {
  if (!t) return null
  return `candidate ${t.version}: ${t.verdict}`
}
