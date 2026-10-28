import type { AttemptIsolationRow } from './api.gen'
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
