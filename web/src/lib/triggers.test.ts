import { describe, it, expect } from 'vitest'
import { journalLineText, ruleSource, triggeredByLine, verdictWords } from './triggers'
import type { LogEntry, TriggeredBy, TriggerVerdict as Verdict } from './api.gen'
import { TRIGGER_CODES } from './consts.gen'

// The verdicts a journal line can carry, in the engine's closed set.
const ALL_VERDICTS: Verdict[] = [
  'skipped',
  'held',
  'started',
  'already_started',
  'refused',
  'failed',
  'gap',
  'positioned',
]

// A line whose every stored field holds a sentinel no wording may repeat:
// the journal stores codes and safe metadata only, and the words of a line
// are fixed sentences, never its stored text.
function entry(fields: Partial<LogEntry> = {}): LogEntry {
  return {
    id: 'sentinel-id',
    at: 1700000000,
    verdict: 'skipped',
    seq: 4242,
    range: [7, 9],
    run_id: 'sentinel-run',
    code: undefined,
    name: 'sentinel-name',
    binding: 'sentinel-binding',
    approved: 'sentinel-approved',
    codes: ['sentinel-code'],
    conditions: [{ index: 0, passed: true }],
    ...fields,
  }
}

const SENTINELS = [
  'sentinel-id',
  'sentinel-run',
  'sentinel-name',
  'sentinel-binding',
  'sentinel-approved',
  'sentinel-code',
]

describe('verdictWords', () => {
  it('names every code', () => {
    // The closed set the engine's journal can hold (design D1, LogEntry.code).
    expect([...TRIGGER_CODES].sort()).toEqual(
      [
        'type',
        'where',
        'own_send',
        'key_missing',
        'key_not_scalar',
        'key_too_long',
        'key_invalid',
        'param_missing',
        'param_not_scalar',
        'param_too_long',
        'param_pattern_mismatch',
        'rate_cap',
        'rate_cap_unknown',
        'start_key_busy',
        'start_key_conflict',
        'check_rule',
        'binding_changed',
      ].sort(),
    )
    for (const code of TRIGGER_CODES) {
      const words = verdictWords(entry({ code }))
      // A known code answers its own fixed sentence, never the fallback.
      expect(words, code).toBeTruthy()
      expect(words, code).not.toBe(`refused (${code})`)
      // The sentence is fixed: the same whatever else the line carries.
      expect(words, code).toBe(verdictWords(entry({ code, verdict: 'held', seq: 1 })))
    }
  })

  it('answers refused (<code>) for a code outside the set', () => {
    expect(verdictWords(entry({ verdict: 'refused', code: 'mystery_kind' }))).toBe(
      'refused (mystery_kind)',
    )
    // A code outside the closed pattern is never echoed back.
    expect(verdictWords(entry({ verdict: 'refused', code: 'No params given' }))).toBe('refused')
  })

  it('names the verdict when the line carries no code, as the engine does', () => {
    const expected: Record<Verdict, string> = {
      skipped: 'skipped',
      held: 'held for the next pass',
      started: 'started a run',
      already_started: 'a run for this event was already started',
      refused: 'refused',
      failed: 'failed',
      gap: 'events 7 to 9 were removed before the rule saw them',
      positioned: 'positioned at the current head of the source',
    }
    for (const verdict of ALL_VERDICTS) {
      expect(verdictWords(entry({ verdict, code: undefined })), verdict).toBe(expected[verdict])
    }
  })

  it('never embeds a stored text field of the entry', () => {
    for (const code of TRIGGER_CODES) {
      const words = verdictWords(entry({ code }))
      for (const sentinel of SENTINELS) expect(words, code).not.toContain(sentinel)
    }
    for (const verdict of ALL_VERDICTS) {
      const words = verdictWords(entry({ verdict, code: undefined }))
      for (const sentinel of SENTINELS) expect(words, verdict).not.toContain(sentinel)
    }
  })
})

describe('journalLineText', () => {
  it('prints the seq, the words, the condition marks, the param, the codes and the run', () => {
    expect(
      journalLineText({
        id: 'i:3:skipped',
        at: 1,
        verdict: 'skipped',
        seq: 3,
        code: 'where',
        conditions: [
          { index: 0, passed: true },
          { index: 1, passed: false },
        ],
      }),
    ).toBe("event 3: the event did not pass the rule's where conditions; conditions #0 passed, #1 failed")
    expect(
      journalLineText({
        id: 'i:4:refused',
        at: 1,
        verdict: 'refused',
        seq: 4,
        code: 'check_rule',
        codes: ['V262', 'V264'],
      }),
    ).toBe('event 4: the rule no longer passes its own checks; codes V262, V264')
    expect(
      journalLineText({ id: 'i:5:started', at: 1, verdict: 'started', seq: 5, run_id: 'r-5' }),
    ).toBe('event 5: started a run; run r-5')
  })

  it('reads a line that carries none of the optional fields', () => {
    // What the wire holds for a `positioned` line: no seq, code, codes or
    // conditions at all.
    const line = { id: 'i:positioned:4', at: 1, verdict: 'positioned', range: [4, 4] } as LogEntry
    expect(journalLineText(line)).toBe('positioned at the current head of the source')
  })
})

describe('ruleSource', () => {
  it('renders connector, account and the watched types', () => {
    const inbox = { connector: 'github', account: 'main', key: '{{body.issue.number}}' }
    expect(ruleSource({ source: { inbox: { ...inbox, types: ['issues'] } } })).toBe(
      'inbox github/main [issues]',
    )
    expect(ruleSource({ source: { inbox } })).toBe('inbox github/main')
    expect(
      ruleSource({ source: { inbox: { ...inbox, types: ['issues', 'pull_request'] } } }),
    ).toBe('inbox github/main [issues, pull_request]')
  })
})

describe('triggeredByLine', () => {
  it('names the rule and the event', () => {
    const by: TriggeredBy = {
      rule: 'triage-new-issues',
      incarnation: 'abc123',
      source: 'inbox',
      connector: 'github',
      account: 'main',
      event_seq: 42,
    }
    expect(triggeredByLine(by)).toBe('started by rule `triage-new-issues` (inbox github/main, event 42)')
  })

  it('renders only the parts that exist', () => {
    expect(triggeredByLine({ rule: 'r', incarnation: 'i', source: 'inbox' })).toBe(
      'started by rule `r` (inbox)',
    )
    expect(triggeredByLine({ rule: 'r', incarnation: 'i', source: 'inbox', connector: 'github' })).toBe(
      'started by rule `r` (inbox github)',
    )
    expect(
      triggeredByLine({ rule: 'r', incarnation: 'i', source: 'inbox', account: 'main', event_seq: 7 }),
    ).toBe('started by rule `r` (inbox main, event 7)')
  })
})
