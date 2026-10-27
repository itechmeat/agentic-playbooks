import type { CandidateTrial } from './api.gen'
import type { VersionInfo } from './types'

// Human-readable origin of a version for the history panel. Which version is
// in use is not part of it: the `current` badge shows that, from `is_current`,
// the one source for it (the old stored `promoted` flag drifted from it).
// A version saved on top of an import nobody approved keeps the import's
// record: it says so after its own origin.
export function provenanceLabel(v: VersionInfo): string {
  const p = v.provenance
  if (p?.imported && p.created_by === 'import') {
    return p.imported.file_name ? `imported from ${p.imported.file_name}` : 'imported'
  }
  const carried = p?.imported ? ', with imported scripts' : ''
  if (p?.created_by !== 'supervisor') return `minor${carried}`
  const kind = p.scope === 'next_runs' ? 'forward patch' : 'patch'
  const parts = [`${kind}: ${p.classification ?? 'unknown'}`]
  if (p.run_id) parts.push(`run ${p.run_id}`)
  return parts.join(', ') + carried
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

// A version an import wrote with scripts runs only after an approval that
// names its digest; until then the history shows this badge.
export function approvalBadge(v: VersionInfo): string | null {
  return v.awaiting_approval ? 'awaiting approval' : null
}

// How to approve a version that awaits it. The command without a digest
// prints the version's digest as it is now (and its scripts) to review
// first; the digest recorded at import time would be stale once a script
// changed. The history lists project playbooks, the command's default scope.
export function approvalHint(id: string, version: string): string {
  return `review and approve it with: apb trust approve ${id} --version ${version} (it prints the digest to name)`
}
