import { describe, expect, it } from 'vitest'
import type { Grade, ReadTrace } from './api.gen'
import type { WfEvent } from './types'
import {
  citeTarget,
  gradeSkippedText,
  hasHostGrade,
  latestGrades,
  levelText,
  levelTone,
  readingLine,
  resultTone,
  tallyLeftOut,
  tallyText,
  verdictText,
  verdictTone,
} from './quality'

const events: WfEvent[] = [
  { seq: 0, ts: 0, type: 'run_started' },
  { seq: 2, ts: 2, type: 'attempt_started', node: 'fix', attempt: 1, transcript: 'attempts/fix-1' },
  { seq: 3, ts: 3, type: 'attempt_finished', node: 'fix', attempt: 1 },
  { seq: 4, ts: 4, type: 'node_finished', node: 'fix' },
  { seq: 7, ts: 7, type: 'node_finished', node: 'fix' },
]

function grade(kind: 'rules' | 'host', created_at: number, id: string): Grade {
  return {
    schema: 1,
    grade_id: id,
    run_id: 'r1',
    playbook: 'p',
    version: '1.0.0',
    task_type: 'default',
    grader: { kind, version: 1 },
    created_at,
    criteria: [],
    reading: { budget_chars: 0, read_chars: 0 },
  }
}

describe('levels', () => {
  it('colours a level and reads it as words', () => {
    expect(levelTone('good')).toBe('pass')
    expect(levelTone('poor')).toBe('fail')
    expect(levelTone('partial')).toBe('gap')
    expect(levelTone('not_enough_data')).toBe('gap')
    expect(levelTone('not_applicable')).toBe('neutral')
    expect(levelText('not_enough_data')).toBe('not enough data')
  })
})

describe('latestGrades', () => {
  it('keeps the newest grade of each grader, newest first', () => {
    const out = latestGrades([grade('rules', 1, 'a'), grade('host', 5, 'b'), grade('rules', 9, 'c')])
    expect(out.map((g) => g.grade_id)).toEqual(['c', 'b'])
  })

  it('keeps the grade listed later on a tie, as the comparison does', () => {
    const out = latestGrades([grade('rules', 5, 'a'), grade('rules', 5, 'b')])
    expect(out.map((g) => g.grade_id)).toEqual(['b'])
  })

  it('tells whether a host graded the run', () => {
    expect(hasHostGrade([grade('rules', 1, 'a')])).toBe(false)
    expect(hasHostGrade([grade('rules', 1, 'a'), grade('host', 2, 'b')])).toBe(true)
  })
})

describe('readingLine', () => {
  it('counts characters, steps and the views read to the end', () => {
    const r: ReadTrace = {
      budget_chars: 200000,
      read_chars: 1234,
      ops: [
        { op: 'outline', ref: 'n:', chars: 10 },
        { op: 'read', ref: 'o:fix', chars: 1224 },
      ],
      views: [
        { view: 'outputs', complete: true, ended: true, read_share: 1 },
        { view: 'journal', complete: true, ended: false, read_share: 0.2 },
      ],
    }
    expect(readingLine(r)).toBe('read 1,234 of 200,000 chars, 2 steps, 1 of 2 views to the end')
    expect(readingLine({ budget_chars: 0, read_chars: 0 })).toBe('read 0 of 0 chars, 0 steps')
  })
})

describe('citeTarget', () => {
  it('points a journal cite at its event', () => {
    expect(citeTarget({ ref: 'j:3' }, events)).toEqual({ label: 'journal #3', seq: 3, path: null })
  })

  it('points a malformed journal cite nowhere', () => {
    for (const ref of ['j:', 'j: 3', 'j:1e1', 'j:0x10', 'j:-1']) {
      expect(citeTarget({ ref }, events).seq).toBeNull()
    }
  })

  it('points an output cite at the last node_finished of the node', () => {
    expect(citeTarget({ ref: 'o:fix', span: [0, 40] }, events)).toEqual({
      label: 'output of fix chars 0-40',
      seq: 7,
      path: null,
    })
  })

  it('points a status cite at the attempt and its status file', () => {
    expect(citeTarget({ ref: 's:fix-1' }, events)).toEqual({
      label: 'status file fix-1',
      seq: 3,
      path: 'agent-status/fix-1.json',
    })
  })

  it('points a transcript cite at the attempt that wrote it and the file', () => {
    expect(citeTarget({ ref: 't:fix-1/session#12' }, events)).toEqual({
      label: 'transcript fix-1 session record 12',
      seq: 2,
      path: 'attempts/fix-1/session.jsonl',
    })
    expect(citeTarget({ ref: 't:gone-1/stderr#0' }, events).seq).toBeNull()
  })

  it('leaves an unknown ref as it is', () => {
    expect(citeTarget({ ref: 'x:1' }, events)).toEqual({ label: 'x:1', seq: null, path: null })
  })
})

describe('comparison', () => {
  it('reads verdicts, results and tallies', () => {
    expect(verdictText('keep_current')).toBe('keep current')
    expect(verdictText('needs_person')).toBe('needs a person')
    expect(verdictTone('promote')).toBe('pass')
    expect(verdictTone('wait')).toBe('gap')
    expect(resultTone('worse')).toBe('fail')
    expect(resultTone('same')).toBe('neutral')
    expect(resultTone('inconclusive')).toBe('gap')
    const t = { good: 4, partial: 1, poor: 0, not_applicable: 2, not_enough_data: 0 }
    expect(tallyText(t)).toBe('4/1/0')
    expect(tallyLeftOut(t)).toBe('2 not applicable')
    expect(tallyLeftOut({ ...t, not_applicable: 0 })).toBeNull()
    const none = { good: 0, partial: 0, poor: 0, not_applicable: 0, not_enough_data: 0 }
    expect(tallyText(none)).toBe('no graded runs yet')
    expect(tallyText(none, 'no graded trials yet')).toBe('no graded trials yet')
    expect(tallyText({ ...none, not_applicable: 3 })).toBe('0/0/0')
  })
})

describe('gradeSkippedText', () => {
  it('puts every model skip code in words', () => {
    for (const code of ['off', 'no_criteria', 'budget', 'send', 'busy', 'state_too_large']) {
      const text = gradeSkippedText(code)
      expect(text).not.toBe(code)
      expect(text).toContain(' ')
    }
    expect(gradeSkippedText('off')).toContain('a provider available')
  })

  it('passes an unknown code through readably', () => {
    expect(gradeSkippedText('some_new_reason')).toBe('some new reason')
  })
})
