import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import raw from '../../../crates/apb-core/tests/fixtures/result_manifest.json?raw'
import { runEventJournal } from './journal'
import { commitLines, goalSummary, goalTone, inboxGapLine, observationText, omittedCommits, trailerNote } from './runoutcome'
import type { NodeCommits, RunGoal } from './api.gen'
import type { WfEvent } from './types'
import {
  byteRangeToUtf16,
  contentNote,
  entryId,
  groupResults,
  liveWord,
  omissionLines,
  rowBadges,
  type ResultRow,
  type SnapshotEntry,
  type SnapshotOmission,
} from './results'
import { compareResultLive, fetchResultEntry, fetchRunResults, resultRawUrl } from './api'

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

  it('notes a commit whose Apb-Run trailer does not name the run', () => {
    const tagged: NodeCommits[] = [
      {
        node: 'impl',
        before: 'aaa',
        after: '111111111111aaaa',
        commits: [
          { sha: '111111111111aaaa', subject: 'ours', apb_run: ['r1'] },
          { sha: '222222222222aaaa', subject: 'bare', apb_run: [] },
          { sha: '333333333333aaaa', subject: 'theirs', apb_run: ['r0', 'r9'] },
          { sha: '444444444444aaaa', subject: 'not read' },
        ],
        omitted: 0,
      },
    ]
    expect(commitLines(tagged, 'r1').map((l) => l.trailer)).toEqual([
      undefined,
      'no Apb-Run trailer',
      'Apb-Run names r0, r9',
      undefined,
    ])
    // Without the run id nothing is claimed.
    expect(trailerNote(tagged[0].commits[1], undefined)).toBeUndefined()
  })

  it('names the attempt that made each commit when its heads were recorded', () => {
    const withHeads = [
      {
        ...commits[0],
        by_attempt: [{ id: 41, number: 2 }, null],
      },
    ]
    expect(commitLines(withHeads).map((l) => l.attempt)).toEqual([2, undefined])
    // An older event carries no heads: nothing is claimed.
    expect(commitLines(commits).map((l) => l.attempt)).toEqual([undefined, undefined])
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
    expect(shown).not.toContain('run-commit-trailer')
    const hidden = render(Panel, { props: { commits: undefined } }).body
    expect(hidden).not.toContain('run-commits')
    const bare: NodeCommits[] = [{ ...commits[0], commits: [{ sha: 'abc', subject: 'bare', apb_run: [] }] }]
    const flagged = render(Panel, { props: { commits: bare, runId: 'r1' } }).body
    expect(flagged).toContain('data-testid="run-commit-trailer"')
    expect(flagged).toContain('no Apb-Run trailer')
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

describe('read_only_violated in the event journal', () => {
  it('lists each changed path by name with its change and counts the ones left out', () => {
    const events: WfEvent[] = [
      {
        seq: 1,
        ts: 1,
        type: 'read_only_violated',
        node: 'scan',
        attempt: 1,
        tier: 'detected',
        changes: [
          { path: 'notes.md', change: 'modified' },
          { path: '.env', change: 'added' },
        ],
        truncated: 4,
      },
      { seq: 2, ts: 2, type: 'read_only_violated', node: 'sh', attempt: 1, changes: [] },
      {
        seq: 3,
        ts: 3,
        type: 'read_only_violated',
        node: 'rb',
        attempt: 1,
        tier: 'detected',
        changes: [{ path: 'a.txt', change: 'modified' }],
        concurrent_with: ['ra'],
      },
    ]
    expect(runEventJournal(events).map((e) => e.note)).toEqual([
      'read-only (detected) node changed the tree: modified notes.md, added .env, 4 more',
      'read-only (detected) node changed the tree: changed',
      'read-only (detected) node changed the tree: modified a.txt (ran beside ra)',
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

// Result snapshots on the run page (`results.ts`): entry ids, ranges, the
// grouping, badges and omission lines, and the client of the results routes.
// The fixture the Rust result store tests read too: its citations and its
// UTF-16 ranges were computed outside both implementations.
const fixture = JSON.parse(raw) as {
  manifest: { entries: SnapshotEntry[]; omitted: SnapshotOmission[] }
  snapshot_id: string
  citations: { ref: string; range?: [number, number] }[]
  ranges: { text: string; bytes: [number, number]; utf16: [number, number] }[]
}

describe('entryId', () => {
  it('renders every entry of the shared fixture as its citations name it', () => {
    const ids = fixture.manifest.entries.map((e) => entryId(e))
    const cited = fixture.citations.map((c) => c.ref.slice(`r:${fixture.snapshot_id}/`.length).replace(/#.*$/, ''))
    expect([...ids].sort()).toEqual([...cited].sort())
  })

  it('encodes the characters a URI component leaves alone, segment by segment', () => {
    expect(entryId({ part: 'file', scope: 'run', path: "it's (a)!/b*.md" })).toBe('file:run:it%27s%20%28a%29%21/b%2A.md')
  })
})

describe('byteRangeToUtf16', () => {
  it('converts the shared fixture byte ranges to UTF-16 offsets', () => {
    for (const r of fixture.ranges) expect(byteRangeToUtf16(r.text, r.bytes)).toEqual(r.utf16)
  })
})

const row = (over: Partial<ResultRow>): ResultRow => ({
  execution: { node: 'report', ordinal: 1 },
  settlement_seq: 10,
  status: 'succeeded',
  snapshot_id: 'a'.repeat(64),
  entries: [],
  omitted: [],
  provenance: { cache_hit: false },
  verified: 'ok',
  ...over,
})

describe('groupResults', () => {
  it('groups rows by node in journal order, then execution, then item in settlement order', () => {
    const visit = { node: 'items', started_seq: 4 }
    const rows = [
      row({ execution: { node: 'report', ordinal: 2 }, settlement_seq: 40 }),
      row({ execution: { node: 'items', ordinal: 1 }, item: { visit, id: 'b' }, settlement_seq: 12 }),
      row({ execution: { node: 'report', ordinal: 1 }, settlement_seq: 9 }),
      row({ execution: { node: 'items', ordinal: 1 }, item: { visit, id: 'a' }, settlement_seq: 11 }),
    ]
    const groups = groupResults(rows)
    expect(groups.map((g) => [g.node, g.count])).toEqual([
      ['report', 2],
      ['items', 2],
    ])
    expect(groups[0].executions.map((e) => e.ordinal)).toEqual([1, 2])
    expect(groups[1].executions[0].rows.map((r) => r.item?.id)).toEqual(['a', 'b'])
  })
})

describe('rowBadges', () => {
  it('names every condition a row carries', () => {
    const badges = rowBadges(
      row({
        entries: [{ entry: 'output', part: 'output', size: 3, digest: 'sha256:x', redacted: true, format: 'text' }],
        omitted: fixture.manifest.omitted,
        incomplete: 'match_limit',
        output_omitted: 'run_limit',
        provenance: { cache_hit: true },
        verified: 'corrupt',
      }),
    ).map((b) => b.text)
    // Two records plus the three the aggregate counts.
    expect(badges).toEqual(['corrupt', 'cache hit', 'redacted', 'incomplete: match limit', 'omitted 5', 'output not stored: run limit'])
  })

  it('has nothing to say about a plain row', () => {
    expect(rowBadges(row({}))).toEqual([])
  })
})

describe('omissionLines', () => {
  it('lists each omission with its reason and the aggregate with its counts', () => {
    expect(omissionLines(fixture.manifest.omitted)).toEqual([
      '3 more: hard link 1, too large 2',
      'file:workspace:bin/tool: binary not supported',
      'file:workspace:.env: secret shaped',
    ])
  })
})

describe('liveWord and contentNote', () => {
  it('says how the live file compares, never what it holds', () => {
    expect(liveWord('same')).toBe('same as the live file')
    expect(liveWord('differs')).toBe('differs from the live file')
    expect(liveWord('not_comparable')).toBe('not comparable: redacted, or not a workspace file')
  })

  it('says when the display cap cut the text', () => {
    const c = { text: '', digest: 'sha256:x', size: 70000, range: [0, 65536] as [number, number], redacted_at_capture: false, redacted_on_read: false }
    expect(contentNote(c)).toBe('Showing bytes 0 to 65536 of 70000. Download for the whole entry.')
    expect(contentNote({ ...c, size: 65536 })).toBeUndefined()
  })
})

describe('results client', () => {
  const fetchMock = vi.fn<typeof fetch>()
  beforeEach(() => vi.stubGlobal('fetch', fetchMock))
  afterEach(() => {
    vi.unstubAllGlobals()
    fetchMock.mockReset()
  })
  const ok = (body: unknown) => new Response(JSON.stringify(body), { status: 200 })
  const H = { headers: { 'x-requested-with': 'apb-dashboard' } }
  // The entry and live reads go through `requestJson`, which keeps a
  // refusal's own sentence.
  const G = { method: 'GET', headers: { 'x-requested-with': 'apb-dashboard' } }
  const snap = 'b'.repeat(64)
  const entry = 'file:workspace:docs/a%20b.md'

  it('reads the view, an entry range and the live comparison under /api/runs/{id}/results', async () => {
    fetchMock.mockResolvedValueOnce(ok({ rows: [], legacy: [] }))
    await fetchRunResults('r 1', 'w1')
    expect(fetchMock).toHaveBeenLastCalledWith('/api/runs/r%201/results?workspace=w1', H)

    // The entry route answers `run_result_read`'s shape: the content sits
    // under `content`, next to the run and the citation it read.
    const content = { text: 'x', digest: 'sha256:' + 'c'.repeat(64), size: 1, range: [0, 1], redacted_at_capture: false, redacted_on_read: false }
    fetchMock.mockResolvedValueOnce(ok({ run_id: 'r1', ref: `r:${snap}/${entry}`, content }))
    await expect(fetchResultEntry('r1', snap, entry, { offset: 65536, limit: 65536 })).resolves.toEqual(content)
    expect(fetchMock).toHaveBeenLastCalledWith(
      `/api/runs/r1/results/${snap}?entry=file%3Aworkspace%3Adocs%2Fa%2520b.md&offset=65536&limit=65536`,
      G,
    )

    // The live route answers `{ live: <word> }`.
    fetchMock.mockResolvedValueOnce(ok({ live: 'differs' }))
    await expect(compareResultLive('r1', snap, entry, 'w1')).resolves.toBe('differs')
    expect(fetchMock).toHaveBeenLastCalledWith(
      `/api/runs/r1/results/${snap}/live?entry=file%3Aworkspace%3Adocs%2Fa%2520b.md&workspace=w1`,
      G,
    )

    expect(resultRawUrl('r1', snap, 'output')).toBe(`/api/runs/r1/results/${snap}/raw?entry=output`)
  })
})
