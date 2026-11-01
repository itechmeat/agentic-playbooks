import { renderAddress, type EdgeCorrespondence, type EdgeOrderChange } from './edgeaddress'

/** One edge present in both versions whose attributes changed, as the diff lists it. */
export interface EdgeChangeLine {
  /** The edge's rendered address in the older version. */
  before: string
  /** Its rendered address in the newer version. */
  after: string
  /** The names of the attributes that changed. */
  changed: string[]
}

/** One source whose outgoing edges are checked in a different order. */
export interface OrderChangeLine {
  node: string
  before: string[]
  after: string[]
}

/**
 * The edges of a diff that changed: rows with changed attributes. A row
 * with only one side (an edge added or removed) is in `edges_added` or
 * `edges_removed` already; an older server sends no rows at all.
 */
export function diffEdgeChanges(diff: { edges?: EdgeCorrespondence[] } | null | undefined): EdgeChangeLine[] {
  return (diff?.edges ?? [])
    .filter((row) => row.changed.length > 0)
    .map((row) => {
      const before = row.before ? renderAddress(row.before) : '(none)'
      const after = row.after ? renderAddress(row.after) : '(none)'
      return { before, after, changed: row.changed }
    })
}

/** The sources whose edges are checked in a different order, as rendered addresses. */
export function diffOrderChanges(
  diff: { edge_order_changed?: EdgeOrderChange[] } | null | undefined,
): OrderChangeLine[] {
  return (diff?.edge_order_changed ?? []).map((c) => ({
    node: c.node,
    before: c.before.map(renderAddress),
    after: c.after.map(renderAddress),
  }))
}

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
