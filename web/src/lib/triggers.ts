// Start rules on the dashboard: the fetchers for the `/api/triggers` routes
// and the pure helpers every trigger surface renders through. Every wire
// type comes from the generated client (`./api.gen`), and the words of a
// journal code from the generated constants (`./consts.gen`), so nothing
// here can drift from the server.
//
// A journal line holds codes and safe metadata only (a code from a closed
// set, a param or path name taken from the rule, digests), never an engine
// sentence, a body value or a param value. Every wording of a line is a
// fixed sentence per code, so nothing stored ever reaches the page as text.

import type {
  ApprovalNeed,
  LogEntry,
  Rule,
  RuleView,
  TestReport,
  TriggerDetail,
  TriggeredBy,
  TriggerVerdict,
} from './api.gen'
import { TRIGGER_CODE_WORDS } from './consts.gen'
import { getJson, jsonHeaders, requestJson } from './api/http'

// --- Fetchers ---

const triggers = '/api/triggers'
const named = (name: string) => `${triggers}/${encodeURIComponent(name)}`

/** Every rule of the project. */
export const fetchTriggers = () =>
  getJson<{ rules: RuleView[] }>(triggers).then((r) => r.rules)

/** One rule with its document and its journal tail. */
export const fetchTrigger = (name: string) => getJson<TriggerDetail>(named(name))

/** What a person would approve for the rule now. */
export const fetchTriggerApproval = (name: string) =>
  getJson<ApprovalNeed>(`${named(name)}/approval`)

/** Creates or replaces the rule from its document. */
export const setTrigger = (name: string, rule: Rule) =>
  requestJson<RuleView>(named(name), {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ rule }),
  })

const postEmpty = (path: string) =>
  requestJson<RuleView>(path, { method: 'POST', headers: jsonHeaders, body: '{}' })

export const enableTrigger = (name: string) => postEmpty(`${named(name)}/enable`)
export const disableTrigger = (name: string) => postEmpty(`${named(name)}/disable`)

/** Approves the exact need the person was shown, unchanged. */
export const approveTrigger = (name: string, shown: ApprovalNeed) =>
  requestJson<RuleView>(`${named(name)}/approve`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ shown }),
  })

/** Removes the rule. The answer names what stayed behind: the state entry
 * or the journal the removal could not delete beneath the anchor (the rule
 * is gone either way). */
export const removeTrigger = (name: string) =>
  requestJson<{
    removed?: boolean
    state_left?: string
    journal_left?: string
  }>(named(name), {
    method: 'DELETE',
    headers: jsonHeaders,
    body: '{}',
  })

/** The dry test of the newest stored event, or of `eventSeq`. */
export const testTrigger = (name: string, eventSeq?: number) =>
  requestJson<TestReport>(`${named(name)}/test`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(eventSeq === undefined ? {} : { event_seq: eventSeq }),
  })

// --- Pure helpers ---

/** Whether `code` passes the closed pattern `[a-z0-9_]{1,64}` the journal
 * stores codes under (the engine's `is_code_text`). */
const isCodeText = (code: string) => /^[a-z0-9_]{1,64}$/.test(code)

/** The words of a codeless line: the verdict alone, as the engine words it. */
function verdictOnlyWords(e: LogEntry): string {
  const words: Record<TriggerVerdict, string> = {
    gap: e.range
      ? `events ${e.range[0]} to ${e.range[1]} were removed before the rule saw them`
      : 'events were removed before the rule saw them',
    positioned: 'positioned at the current head of the source',
    started: 'started a run',
    already_started: 'a run for this event was already started',
    held: 'held for the next pass',
    refused: 'refused',
    skipped: 'skipped',
    failed: 'failed',
  }
  return words[e.verdict]
}

/** The words of one journal line, the twin of the engine's
 * `verdict_words`: the fixed sentence of a known code, `refused (<code>)`
 * for any other code that passes the closed pattern (`refused` otherwise),
 * the verdict's words when the line carries no code. Never a stored
 * sentence and never a stored text field: an engine refusal's own sentence
 * can name a param value. */
export function verdictWords(e: LogEntry): string {
  if (e.code !== undefined && e.code !== null && e.code !== '') {
    const known = (TRIGGER_CODE_WORDS as Record<string, string>)[e.code]
    if (known !== undefined) return known
    return isCodeText(e.code) ? `refused (${e.code})` : 'refused'
  }
  return verdictOnlyWords(e)
}

/** One journal line as every surface prints it, the twin of the engine's
 * `journal_line_text`: the event's seq, the verdict words, the condition
 * marks (index and passed or failed, never the literal), the param name,
 * the validator codes and the run id, each only when the line has it. */
export function journalLineText(e: LogEntry): string {
  const words = verdictWords(e)
  const parts = [e.seq != null ? `event ${e.seq}: ${words}` : words]
  if (e.conditions?.length) {
    const marks = e.conditions.map((m) => `#${m.index} ${m.passed ? 'passed' : 'failed'}`)
    parts.push(`conditions ${marks.join(', ')}`)
  }
  if (e.name) parts.push(`param ${e.name}`)
  if (e.codes?.length) parts.push(`codes ${e.codes.join(', ')}`)
  if (e.run_id) parts.push(`run ${e.run_id}`)
  return parts.join('; ')
}

/** `inbox github/main [issues]` from a rule document's source. */
export function ruleSource(r: Pick<Rule, 'source'>): string {
  const inbox = r.source.inbox
  const types = inbox.types?.length ? ` [${inbox.types.join(', ')}]` : ''
  return `inbox ${inbox.connector}/${inbox.account}${types}`
}

/** `started by rule \`x\` (inbox github/main, event 42)`: only the parts
 * that exist. The twin of the engine's `triggered_by_line`. */
export function triggeredByLine(by: TriggeredBy): string {
  const target = [by.connector, by.account].filter((v) => v).join('/')
  const where = target ? `${by.source} ${target}` : by.source
  const inner = by.event_seq != null ? `${where}, event ${by.event_seq}` : where
  return `started by rule \`${by.rule}\` (${inner})`
}
