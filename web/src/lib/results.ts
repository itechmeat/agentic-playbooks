// Result snapshots on the run page: the rows of `GET /api/runs/{id}/results`
// grouped by node, execution and list item, the entry ids a citation names,
// the badges and omission lines of a row, the word a live comparison
// answers, and byte ranges of stored text as UTF-16 offsets. Pure helpers,
// no UI state; the panel is `RunResultsPanel.svelte`.

import type {
  CommitAttempt,
  EntryContent,
  EntryPart,
  EntryView,
  IncompleteReason,
  LegacyRefRow,
  LiveComparison,
  NotStoredRow,
  OmissionReason,
  OmittedItem,
  OmittedMore,
  OutputOmitted,
  ResultFormat,
  ResultRow,
  ResultScope,
  ResultsView,
  SnapshotEntry,
  SnapshotOmission,
  SnapshotProvenance,
  Verified,
} from './api.gen'
import { RESULT_DISPLAY_CAP, RESULT_INCOMPLETE_REASONS, RESULT_OMISSION_REASONS, RESULT_OUTPUT_OMITTED } from './consts.gen'

// Every reason this apb writes is a name the generated unions know; a
// newer apb's name reads as `unknown` there.
RESULT_OMISSION_REASONS satisfies readonly OmissionReason[]
RESULT_INCOMPLETE_REASONS satisfies readonly IncompleteReason[]
RESULT_OUTPUT_OMITTED satisfies readonly OutputOmitted[]

// The shapes come from the Rust side (`ts_contract`): the view of
// `apb_engine::result_view` and the manifest parts of
// `apb_core::result_store`.
export type {
  CommitAttempt,
  EntryContent,
  EntryPart,
  EntryView,
  IncompleteReason,
  LegacyRefRow,
  LiveComparison,
  NotStoredRow,
  OmissionReason,
  OmittedItem,
  OmittedMore,
  OutputOmitted,
  ResultFormat,
  ResultRow,
  ResultScope,
  ResultsView,
  SnapshotEntry,
  SnapshotOmission,
  SnapshotProvenance,
  Verified,
} from './api.gen'

/// The display cap of one inline read (`apb_engine::result_view::DISPLAY_CAP`).
export const DISPLAY_CAP = RESULT_DISPLAY_CAP

// The paths a capture's scrub changed are stored as numbered placeholders.
const REDACTED_PATH_PREFIX = '<redacted path '

export function isRedactedPath(path: string | undefined): boolean {
  return path !== undefined && path.startsWith(REDACTED_PATH_PREFIX)
}

// Every byte but the RFC 3986 unreserved characters, as the core encodes a
// path segment (`encodeURIComponent` leaves `!'()*` alone).
function encodeSegment(seg: string): string {
  return encodeURIComponent(seg).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`)
}

// The id of an entry inside its snapshot: `output`, `fields` or
// `file:<scope>:<path>`, the path percent-encoded segment by segment.
export function entryId(e: Pick<SnapshotEntry, 'part' | 'scope' | 'path'>): string {
  if (e.part !== 'file') return e.part
  return `file:${e.scope ?? ''}:${(e.path ?? '').split('/').map(encodeSegment).join('/')}`
}

// The readable name of an entry or an omitted item: the path stays as
// stored (it is shown as text, never decoded markup).
export function entryName(e: { part?: EntryPart; scope?: string; path?: string }): string {
  if (e.part === 'file' || (e.part === undefined && e.path !== undefined)) {
    return `file:${e.scope ?? ''}:${e.path ?? ''}`
  }
  return e.part ?? 'unknown'
}

// A byte range `[a, b)` of UTF-8 text as UTF-16 offsets into `text`, for
// highlighting a cited span in the browser.
export function byteRangeToUtf16(text: string, [a, b]: [number, number]): [number, number] {
  const enc = new TextEncoder()
  const at = (bytes: number): number => {
    let used = 0
    let units = 0
    for (const ch of text) {
      if (used >= bytes) break
      used += enc.encode(ch).length
      units += ch.length
    }
    return units
  }
  return [at(a), at(b)]
}

const words = (name: string) => name.replace(/_/g, ' ')

function isMore(o: SnapshotOmission): o is OmittedMore {
  return typeof (o as OmittedMore).more === 'number'
}

// How many things a row's capture left out: each record, plus the count the
// aggregate record holds.
export function omittedCount(omitted: SnapshotOmission[]): number {
  return omitted.reduce((n, o) => n + (isMore(o) ? o.more : 1), 0)
}

// One line per omission: `<entry>: <reason>`, and the aggregate as
// `<n> more: <reason> <count>, ...`.
export function omissionLines(omitted: SnapshotOmission[]): string[] {
  return omitted.map((o) => {
    if (isMore(o)) {
      const parts = Object.entries(o.reasons)
        .sort(([x], [y]) => (x < y ? -1 : x > y ? 1 : 0))
        .map(([r, n]) => `${words(r)} ${n}`)
      return `${o.more} more${parts.length ? `: ${parts.join(', ')}` : ''}`
    }
    return `${entryName(o)}: ${words(o.reason)}`
  })
}

export type BadgeTone = 'bad' | 'warn' | 'plain'
export interface Badge {
  text: string
  tone: BadgeTone
}

// The conditions a row carries, worst first.
export function rowBadges(row: ResultRow): Badge[] {
  const out: Badge[] = []
  if (row.verified === 'corrupt') out.push({ text: 'corrupt', tone: 'bad' })
  if (row.verified === 'missing') out.push({ text: 'missing', tone: 'bad' })
  if (row.provenance.cache_hit) out.push({ text: 'cache hit', tone: 'plain' })
  if (row.entries.some((e) => e.redacted || e.redacted_on_read)) out.push({ text: 'redacted', tone: 'warn' })
  if (row.incomplete) out.push({ text: `incomplete: ${words(row.incomplete)}`, tone: 'warn' })
  const n = omittedCount(row.omitted)
  if (n > 0) out.push({ text: `omitted ${n}`, tone: 'warn' })
  if (row.output_omitted) out.push({ text: `output not stored: ${words(row.output_omitted)}`, tone: 'warn' })
  return out
}

export interface ExecutionGroup {
  ordinal: number
  rows: ResultRow[]
}
export interface NodeGroup {
  node: string
  count: number
  executions: ExecutionGroup[]
}

// Rows by node (in the order the journal first settled each), then by
// execution ordinal, then in settlement order (list items).
export function groupResults(rows: ResultRow[]): NodeGroup[] {
  const sorted = [...rows].sort((x, y) => x.settlement_seq - y.settlement_seq)
  const byNode = new Map<string, Map<number, ResultRow[]>>()
  for (const r of sorted) {
    const execs = byNode.get(r.execution.node) ?? new Map<number, ResultRow[]>()
    byNode.set(r.execution.node, execs)
    const list = execs.get(r.execution.ordinal) ?? []
    execs.set(r.execution.ordinal, list)
    list.push(r)
  }
  return [...byNode].map(([node, execs]) => ({
    node,
    count: [...execs.values()].reduce((n, l) => n + l.length, 0),
    executions: [...execs]
      .sort(([x], [y]) => x - y)
      .map(([ordinal, list]) => ({ ordinal, rows: list })),
  }))
}

// The recorded digests without stored bytes, in journal order.
export function legacyLines(legacy: LegacyRefRow[]): string[] {
  return [...legacy]
    .sort((x, y) => x.settlement_seq - y.settlement_seq)
    .map((l) => `${l.execution.node} #${l.execution.ordinal} · file:${l.scope}:${l.path} · ${shortDigest(l.digest)}`)
}

// One line per settlement whose snapshot was not stored:
// `<node> #<execution>[ item <id>]: <why>`, in the order recorded.
export function notStoredLines(rows: NotStoredRow[]): string[] {
  return rows.map((r) => `${r.node} #${r.execution}${r.item !== undefined ? ` item ${r.item}` : ''}: ${r.reason}`)
}

export function shortDigest(digest: string): string {
  return digest.startsWith('sha256:') ? digest.slice(0, 'sha256:'.length + 12) : digest.slice(0, 12)
}

// Only a stored workspace file has a live copy to compare with.
export function canCompareLive(e: Pick<SnapshotEntry, 'part' | 'scope'>): boolean {
  return e.part === 'file' && e.scope === 'workspace'
}

export function liveWord(c: LiveComparison | string): string {
  switch (c) {
    case 'same':
      return 'same as the live file'
    case 'differs':
      return 'differs from the live file'
    case 'missing':
      return 'the live file is missing'
    case 'unreadable':
      return 'the live file is unreadable'
    case 'not_comparable':
      return 'not comparable: redacted, or not a workspace file'
    default:
      return words(c)
  }
}

// Said under a text the display cap cut.
export function contentNote(c: EntryContent): string | undefined {
  const [a, b] = c.range
  if (a === 0 && b >= c.size) return undefined
  return `Showing bytes ${a} to ${b} of ${c.size}. Download for the whole entry.`
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`
  return `${(n / (1024 * 1024)).toFixed(1)} MiB`
}
