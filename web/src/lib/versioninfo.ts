import type { VersionInfo } from './types'

// Human-readable version-provenance string for the history panel.
export function provenanceLabel(v: VersionInfo): string {
  const parts: string[] = []
  if (v.provenance) {
    parts.push(`patch: ${v.provenance.classification ?? 'unknown'}`)
    parts.push(v.provenance.promoted ? 'promoted' : 'not promoted')
    if (v.provenance.run_id) parts.push(`run ${v.provenance.run_id}`)
  } else {
    parts.push('minor')
    if (v.is_current) parts.push('current')
  }
  return parts.join(', ')
}
