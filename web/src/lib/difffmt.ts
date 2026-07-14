export interface DiffLine {
  kind: 'add' | 'del' | 'ctx' | 'meta'
  text: string
}

// Parses unified-diff text (the yaml_diff field from VersionDiff) into an array
// of lines with a marker type. Hunk-header lines (@@) and file-header lines
// (---/+++) are marked as meta; added/removed/context lines are classified by
// their first character. The marker itself is stripped from text; meta lines
// are kept in full.
export function formatDiff(diff: string): DiffLine[] {
  if (!diff) return []
  const out: DiffLine[] = []
  for (const raw of diff.split('\n')) {
    if (raw.startsWith('@@')) {
      out.push({ kind: 'meta', text: raw })
    } else if (raw.startsWith('---') || raw.startsWith('+++')) {
      out.push({ kind: 'meta', text: raw })
    } else if (raw.startsWith('+')) {
      out.push({ kind: 'add', text: raw.slice(1) })
    } else if (raw.startsWith('-')) {
      out.push({ kind: 'del', text: raw.slice(1) })
    } else if (raw.startsWith(' ')) {
      out.push({ kind: 'ctx', text: raw.slice(1) })
    } else {
      out.push({ kind: 'ctx', text: raw })
    }
  }
  return out
}
