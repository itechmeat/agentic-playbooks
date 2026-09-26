import type { VersionInfo } from './types'

// Human-readable origin of a version for the history panel. Which version is
// in use is not part of it: the `current` badge shows that, from `is_current`,
// the one source for it (the old stored `promoted` flag drifted from it).
export function provenanceLabel(v: VersionInfo): string {
  const p = v.provenance
  if (p?.created_by !== 'supervisor') return 'minor'
  const parts = [`patch: ${p.classification ?? 'unknown'}`]
  if (p.run_id) parts.push(`run ${p.run_id}`)
  return parts.join(', ')
}
