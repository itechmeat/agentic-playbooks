import { describe, expect, it } from 'vitest'
import { runEventJournal } from './journal'
import { commitLines, goalSummary, goalTone, inboxGapLine, observationText, omittedCommits } from './runoutcome'
import type { NodeCommits, RunGoal } from './api.gen'
import type { WfEvent } from './types'

const commits: NodeCommits[] = [
  {
    node: 'impl',
    before: 'aaa',
    after: '0123456789abcdef',
    commits: [
      { sha: '0123456789abcdef', subject: 'fix the parser' },
      { sha: 'fedcba9876543210', subject: 'add a failing test' },
    ],
    omitted: 3,
  },
]

describe('commitLines', () => {
  it('lists every commit with a short sha, per node', () => {
    expect(commitLines(commits)).toEqual([
      { node: 'impl', sha: '0123456789abcdef', short: '0123456789ab', subject: 'fix the parser' },
      { node: 'impl', sha: 'fedcba9876543210', short: 'fedcba987654', subject: 'add a failing test' },
    ])
    expect(omittedCommits(commits)).toBe(3)
  })

  it('is empty for a run without commits (the block stays hidden)', () => {
    expect(commitLines(undefined)).toEqual([])
    expect(omittedCommits(undefined)).toBe(0)
  })
})

describe('artifacts_committed in the event journal', () => {
  it('notes the count and the newest commit', () => {
    const events: WfEvent[] = [
      { seq: 1, ts: 1, type: 'artifacts_committed', node: 'impl', before: 'a', after: 'b', commits: commits[0].commits, omitted: 3 },
    ]
    expect(runEventJournal(events)[0].note).toBe('5 commits: 0123456789ab fix the parser')
  })
})

describe('RunOutcomePanel', () => {
  it('renders the commits, and nothing for a run without them', async () => {
    const { render } = await import('svelte/server')
    const Panel = (await import('./RunOutcomePanel.svelte')).default
    const shown = render(Panel, { props: { commits } }).body
    expect(shown).toContain('data-testid="run-commits"')
    expect(shown).toContain('fix the parser')
    expect(shown).toContain('and 3 more')
    const hidden = render(Panel, { props: { commits: undefined } }).body
    expect(hidden).not.toContain('run-commits')
  })
})

const goal: RunGoal = {
  statement: 'Ship the parser fix',
  enforce: true,
  checked: true,
  criteria: [
    { index: 0, description: 'tests pass', check: 'script', status: 'passed' },
    { index: 1, description: 'the answer says DONE', check: 'marker', status: 'failed', detail: 'marker `DONE` not found' },
    { index: 2, description: 'a person reads the diff', check: 'manual', status: 'manual' },
  ],
  passed: 1,
  failed: 1,
  manual: 1,
}

describe('goal', () => {
  it('summarises the results, and says when nothing was checked yet', () => {
    expect(goalSummary(goal)).toBe('1 passed · 1 failed · 1 to confirm · enforced')
    const pending: RunGoal = {
      ...goal,
      enforce: false,
      checked: false,
      passed: 0,
      failed: 0,
      criteria: goal.criteria.map((c) => ({ ...c, status: c.check === 'manual' ? 'manual' : 'pending', detail: undefined })),
    }
    expect(goalSummary(pending)).toBe('not checked yet · 3 criteria')
    // A statement-only goal is never pending.
    expect(goalSummary({ ...pending, criteria: [] })).toBe('no criteria to check')
    expect(['passed', 'failed', 'error', 'manual', 'pending'].map(goalTone)).toEqual([
      'pass',
      'fail',
      'fail',
      'neutral',
      'neutral',
    ])
  })

  it('renders checked criteria with details and manual ones as a checklist', async () => {
    const { render } = await import('svelte/server')
    const Panel = (await import('./RunOutcomePanel.svelte')).default
    const body = render(Panel, { props: { goal } }).body
    expect(body).toContain('data-testid="run-goal"')
    expect(body).toContain('Ship the parser fix')
    expect(body).toContain('marker `DONE` not found')
    expect(body).toContain('data-testid="run-goal-checklist"')
    expect(body).toContain('a person reads the diff')
    // Only the two checked criteria are rows; the manual one is in the checklist.
    expect(body.match(/data-testid="run-goal-criterion"/g)?.length).toBe(2)
    // A goal without manual criteria has no checklist, and no goal renders nothing.
    const auto = render(Panel, { props: { goal: { ...goal, criteria: goal.criteria.slice(0, 2), manual: 0 } } }).body
    expect(auto).not.toContain('run-goal-checklist')
    expect(render(Panel, { props: {} }).body).not.toContain('run-goal')
  })

  it('notes goal_checked in the event journal', () => {
    const events: WfEvent[] = [
      { seq: 1, ts: 1, type: 'goal_checked', index: 1, description: 'x', check: 'marker', status: 'failed', detail: 'not found' },
      { seq: 2, ts: 2, type: 'goal_checked', index: 0, description: 'y', check: 'script', status: 'passed' },
    ]
    expect(runEventJournal(events).map((e) => e.note)).toEqual(['criterion 2: failed (not found)', 'criterion 1: passed'])
  })
})

describe('protected_paths_modified in the event journal', () => {
  it('lists each change and any path that could not be restored', () => {
    const events: WfEvent[] = [
      {
        seq: 1,
        ts: 1,
        type: 'protected_paths_modified',
        node: 'fix',
        attempt: 1,
        changes: [
          { path: 'tests/a.rs', change: 'modified' },
          { path: 'tests/b.rs', change: 'added' },
        ],
      },
      { seq: 2, ts: 2, type: 'protected_paths_modified', node: 'fix', attempt: 2, changes: [{ path: 'x', change: 'deleted' }], restore_failed: ['x'], kept_copies: '/r/protect/fix/2' },
    ]
    expect(runEventJournal(events).map((e) => e.note)).toEqual([
      'protected: modified tests/a.rs, added tests/b.rs',
      'protected: deleted x; not restored: x (copies kept in /r/protect/fix/2)',
    ])
  })
})

describe('inboxGapLine', () => {
  it('names one lost event or a range of them', () => {
    const gap = { node: 'watch', connector: 'echo-hooks', account: 'main', from: 4, through: 4 }
    expect(inboxGapLine(gap)).toBe(
      'echo-hooks/main: unread event 4 was dropped by inbox retention before it was read (found by watch)',
    )
  })

  it('notes inbox_gap in the event journal with the same words', () => {
    const events: WfEvent[] = [
      { seq: 1, ts: 1, type: 'inbox_gap', node_id: 'watch', connector: 'echo-hooks', account: 'main', from: 3, through: 6 },
    ]
    expect(runEventJournal(events).map((e) => e.note)).toEqual([
      'echo-hooks/main: unread events 3-6 were dropped by inbox retention before they were read (found by watch)',
    ])
  })
})

describe('baseline and notes events in the event journal', () => {
  const num = (n: number) => ({ state: 'value', value: { type: 'number', value: n } })
  it('notes a snapshot, a comparison and an attempt\'s notes', () => {
    const events: WfEvent[] = [
      { seq: 1, ts: 1, type: 'baseline_observed', check: 'unowned', phase: 'before', status: 'observed', observations: { count: num(37) } },
      { seq: 2, ts: 2, type: 'baseline_observed', check: 'tests', phase: 'after', status: 'unknown', reason: 'the check changed the working tree' },
      { seq: 3, ts: 3, type: 'baseline_compared', check: 'unowned', observation: 'count', rule: 'equals 0', before: num(37), after: num(40), verdict: 'not_met', regressed: true },
      { seq: 4, ts: 4, type: 'attempt_notes', node: 'fix', attempt: 1, findings: [{}], warnings: [{}, {}], dropped: 1 },
      { seq: 5, ts: 5, type: 'baseline_compared', check: 'tests', observation: 'npm_test', rule: 'no_new_failures', verdict: 'unknown', detail: 'after: the check changed the working tree' },
    ]
    expect(runEventJournal(events).map((e) => e.note)).toEqual([
      'unowned before: observed (count 37)',
      'tests after: unknown (the check changed the working tree)',
      'unowned/count: 37 -> 40, equals 0: not met, worse than before',
      '1 finding, 2 warnings, 1 dropped as malformed',
      'tests/npm_test: unknown -> unknown, no_new_failures: unknown (after: the check changed the working tree)',
    ])
  })

  it('reads an unknown or a list observation without guessing a value', () => {
    expect(observationText({ state: 'unknown', reason: 'cut short' })).toBe('unknown')
    expect(observationText({ state: 'value', value: { type: 'list', value: ['a', 'b'] } })).toBe('2 items')
    expect(observationText(undefined)).toBe('unknown')
  })
})
