import type { AttemptIsolationRow, ReadOnlyRow } from './api.gen'
import {
  ISOLATION_CODE_TEXTS,
  ISOLATION_ENV_PASSED_PREFIX,
  ISOLATION_ENV_PASSED_TEXT,
  ISOLATION_REASON_TEXTS,
} from './consts.gen'

// What one isolated attempt held and did not, worded from the code tables
// apb-core exports, so the run page says exactly what `apb runs <id>` and
// `apb doctor --run` say.

/** One limit or hold: its code id and its sentence. */
export interface IsolationLine {
  code: string
  text: string
}

/** The sentence of a hold or limit code, the id itself when unknown (newer apb). */
export function isolationCodeText(code: string): string {
  if (code.startsWith(ISOLATION_ENV_PASSED_PREFIX)) {
    return `${ISOLATION_ENV_PASSED_TEXT} (${code.slice(ISOLATION_ENV_PASSED_PREFIX.length)})`
  }
  return (ISOLATION_CODE_TEXTS as Record<string, string>)[code] ?? code
}

/**
 * A journaled reason (`code (detail)` or a bare code) with its sentence:
 * why a best_effort attempt ran without a sandbox.
 */
export function isolationReasonText(reason: string): IsolationLine {
  const code = reason.split(' ', 1)[0] ?? reason
  const text = (ISOLATION_REASON_TEXTS as Record<string, string>)[code]
  return { code: reason, text: text ?? reason }
}

/** The badge of an attempt: level, sandbox or not, network. */
export function isolationBadge(row: AttemptIsolationRow): string {
  const iso = row.isolation
  const where = isSandboxed(row) ? 'sandbox' : 'no sandbox'
  return `${iso.level} · ${where} · network ${iso.network}`
}

/** Whether the attempt ran in a sandbox: only a known backend counts (fails closed). */
export function isSandboxed(row: AttemptIsolationRow): boolean {
  return row.isolation.backend === 'bwrap'
}

/** The attempt's limits, each with its sentence, in journal order. */
export function isolationLimits(row: AttemptIsolationRow): IsolationLine[] {
  return row.isolation.limits.map((code) => ({ code, text: isolationCodeText(code) }))
}

/** One attempt on the run page: its isolation record, its read-only check, or both. */
export interface AttemptEntry {
  node: string
  item?: string
  attempt: number
  isolation?: AttemptIsolationRow
  readOnly?: ReadOnlyRow
}

/**
 * The isolated attempts and the read-only checks as one list: a check of
 * an attempt that has an isolation record joins its row, any other check
 * (a script, an attempt without isolation) is a row of its own after them.
 */
export function attemptEntries(rows: AttemptIsolationRow[], readOnly: ReadOnlyRow[]): AttemptEntry[] {
  const out: AttemptEntry[] = rows.map((r) => ({ node: r.node, item: r.item, attempt: r.attempt, isolation: r }))
  for (const ro of readOnly) {
    const same = out.find((e) => e.isolation && !e.readOnly && e.node === ro.node && e.item === ro.item && e.attempt === ro.attempt)
    if (same) same.readOnly = ro
    else out.push({ node: ro.node, item: ro.item, attempt: ro.attempt, readOnly: ro })
  }
  return out
}

/** The badge of a read-only check: its tier, and the limit of a partial one. */
export function readOnlyBadge(row: ReadOnlyRow): string {
  switch (row.tier) {
    case 'enforced':
      return 'read-only · enforced'
    case 'detected_partial':
      return `read-only · detected, partial (${row.partial ?? 'entries'} limit)`
    default:
      return 'read-only · detected'
  }
}

/** What the check covered, in one sentence. */
export function readOnlyText(row: ReadOnlyRow): string {
  const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`
  const over = row.entries || row.bytes ? ` over ${count(row.entries ?? 0, 'entry', 'entries')}, ${count(row.bytes ?? 0, 'byte', 'bytes')}` : ''
  switch (row.tier) {
    case 'enforced':
      return 'The sandbox bound the tree read-only: a write fails at the OS level.'
    case 'detected_partial':
      return `Checked after the attempt${over}; a limit stopped the check short, so a change past it goes unseen.`
    default:
      return `Checked after the attempt${over}.`
  }
}

/** Whether the badge warns: the node changed the tree, or the check was partial. */
export function readOnlyWarns(row: ReadOnlyRow): boolean {
  return Boolean(row.violated) || row.tier === 'detected_partial'
}
