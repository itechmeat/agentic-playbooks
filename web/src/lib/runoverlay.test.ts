import { describe, expect, it } from 'vitest'
import type { EdgeAddress, EdgeRun, JudgeNodeAnswers, NotRun, RunGraph, SegmentMapping } from './api.gen'
import {
  alignmentText,
  classWord,
  edgeLook,
  edgeRunLabel,
  graphOf,
  graphWithheld,
  judgeCard,
  judgeEdgeText,
  latestJudges,
  nodeCounts,
  notRunTitle,
  overlayBanner,
  projectOverlay,
  runEdgeLooks,
  segmentBanner,
} from './runoverlay'

const d = (from: string, to: string, ordinal = 1): EdgeAddress => ({ kind: 'derived', from, to, ordinal })
const r = (a: EdgeAddress) => (a.kind === 'authored' ? `id:${a.id}` : `e:${a.from}>${a.to}#${a.ordinal}`)

const edge = (segment: number, a: EdgeAddress, state: EdgeRun extends infer T ? Partial<T> : never): EdgeRun =>
  ({
    segment,
    edge: a,
    rendered: r(a),
    from: a.kind === 'derived' ? a.from : 'a',
    to: a.kind === 'derived' ? a.to : 'b',
    ...state,
  }) as EdgeRun

const seg = (index: number, known = true) => ({
  index,
  from_seq: index * 10,
  to_seq: null,
  opened_by: index ? 'patch' : 'start',
  digest: known ? `d${index}` : null,
  known,
  reason: known ? null : 'definition_not_stored',
  mismatch_recorded: false,
})

const graph = (over: Partial<RunGraph> = {}): RunGraph => ({
  segments: [seg(0)],
  shown_segment: 0,
  nodes: [],
  edges: [],
  traversals: [],
  judge_nodes: [],
  judge_edges: [],
  not_run: [],
  ended: true,
  ...over,
})

describe('edge states', () => {
  it('shows a count only above one and never on a not established edge', () => {
    expect(edgeLook({ state: 'taken', count: 1 })).toEqual({ state: 'taken', title: 'taken once' })
    expect(edgeRunLabel(edgeLook({ state: 'taken', count: 3 }))).toBe('x3')
    const ne = edgeLook({ state: 'not_established', reason: 'routing_not_recorded' })
    expect(ne.count).toBeUndefined()
    expect(ne.title).toBe('not established: routing_not_recorded')
    expect(edgeRunLabel(ne)).toBeUndefined()
    expect(edgeLook({ state: 'not_taken' }).state).toBe('not_taken')
  })

  it('reads two edges between the same nodes apart, from the shown segment only', () => {
    const cond = { kind: 'authored', id: 'ok' } as EdgeAddress
    const fallback = d('a', 'b', 2)
    const g = graph({
      segments: [seg(0), seg(1)],
      shown_segment: 1,
      edges: [
        edge(0, cond, { state: 'taken', count: 2 } as never),
        edge(1, cond, { state: 'not_taken' } as never),
        edge(1, fallback, { state: 'taken', count: 1 } as never),
      ],
    })
    const looks = runEdgeLooks(g, new Set(['b']))
    expect(looks.get('id:ok')?.state).toBe('not_taken')
    expect(looks.get(r(fallback))?.state).toBe('taken')
    // The live decoration rides on a taken edge only.
    expect(looks.get(r(fallback))?.live).toBe(true)
    expect(looks.get('id:ok')?.live).toBeUndefined()
    expect(runEdgeLooks({ ...g, shown_segment: null }).size).toBe(0)
  })

  it('labels a judge edge with p against min_p, or unknown', () => {
    const a = d('j', 'x')
    const g = graph({
      edges: [edge(0, a, { state: 'taken', count: 1 } as never)],
      judge_edges: [
        { node: 'j', execution: 1, seq: 4, edge: a, rendered: r(a), p: 0.4, min_p: 0.7, mode: 'enforce', applied: true, taken: false, label: 'p 0.40 < 0.70' },
        { node: 'j', execution: 2, seq: 9, edge: a, rendered: r(a), p: 0.82, min_p: 0.7, mode: 'enforce', applied: true, taken: true, label: 'p 0.82 >= 0.70' },
      ],
    })
    expect(runEdgeLooks(g).get(r(a))?.judge).toBe('judge p 0.82 >= 0.70')
    expect(judgeEdgeText({ label: 'unknown', p: null })).toBe('unknown')
  })

  it('banners several definitions and an unknown one', () => {
    expect(segmentBanner(graph())).toBeNull()
    expect(segmentBanner(graph({ segments: [seg(0), seg(1)] }))).toMatch(/2 definitions/)
    expect(segmentBanner(graph({ segments: [seg(0, false)] }))).toMatch(/unknown/)
    // A resume over the same bytes is one definition: no banner.
    expect(segmentBanner(graph({ segments: [seg(0), { ...seg(1), digest: 'd0', opened_by: 'resume' }] }))).toBeNull()
  })

  it('handles a withheld graph', () => {
    expect(graphOf('[secrets_unresolved]')).toBeNull()
    expect(graphWithheld('[secrets_unresolved]')).toBe(true)
    expect(graphOf(null)).toBeNull()
  })
})

describe('nodes', () => {
  it('counts executions of a looped node with its statuses in the tooltip', () => {
    const c = nodeCounts({
      node: 'build',
      executions: 3,
      open: 1,
      cancelled: 0,
      by_status: { failed: 1, succeeded: 1 },
      status: 'running',
      last_status: 'succeeded',
      in_definition: true,
    })
    expect(c.badge).toBe('x3')
    expect(c.title).toBe('3 executions; 1 open; ended: failed 1, succeeded 1; latest ended succeeded')
  })

  it('words a not-run statement with its class and entry facts', () => {
    const n: NotRun = {
      node: 'deploy',
      class: 'observed',
      code: 'routed_elsewhere',
      detail: 'review routed to fix',
      inputs: [
        { segment: 0, entry: { kind: 'edge', edge: d('review', 'deploy'), rendered: 'e:review>deploy#1', from: 'review' }, class: 'inferred', code: 'condition_not_matched', detail: 'verdict was not ok' },
        { segment: 0, entry: { kind: 'policy', policy: 'defaults', source: null }, class: 'unknown', code: 'cannot_tell', detail: 'no record' },
      ],
    }
    expect(classWord('observed')).toBe('seen')
    expect(notRunTitle(n).split('\n')).toEqual([
      'not executed (seen): review routed to fix',
      '- edge e:review>deploy#1 from review: inferred, verdict was not ok',
      '- policy defaults: unknown, no record',
    ])
  })
})

describe('judge answers', () => {
  const judge = (over: Partial<JudgeNodeAnswers> = {}): JudgeNodeAnswers => ({
    node: 'j',
    execution: 1,
    segment: 0,
    status: 'succeeded',
    decided_by: 'typesafe/jev',
    reason: null,
    fallback: 'fail',
    evidence: [{ seq: 3, source: 'native', mode: 'enforce', applied: true, provider: 'typesafe', model: 'jev', error: null }],
    questions: [],
    ...over,
  })

  it('shows the comparison, the routed value apart, and at most three lines', () => {
    const card = judgeCard(
      judge({
        questions: [
          {
            id: 'verdict',
            kind: 'choice',
            answers: [
              {
                source: 'native',
                value: 'ok',
                p: 0.95,
                confidence: 0.3,
                invalid: null,
                comparison: { measure: 'confidence', observed: 0.3, passed: false, band: null, label: 'confidence 0.30 < 0.70: takes below' },
              },
            ],
            routed_value: 'below',
          },
          { id: 'risky', kind: 'noul', answers: [{ source: 'native', value: true, p: 0.82, confidence: null, invalid: null, comparison: null }], routed_value: true },
          { id: 'size', kind: 'score', answers: [{ source: 'native', value: 2.4, p: null, confidence: null, invalid: null, comparison: null }], routed_value: 'high' },
          { id: 'extra', kind: 'noul', answers: [{ source: 'emulation', value: false, p: 0.1, confidence: null, invalid: null, comparison: null }], routed_value: false },
        ],
      }),
    )
    expect(card.lines).toEqual(['verdict: ok (confidence 0.30 < 0.70: takes below)', 'risky: true', 'size: 2.40'])
    expect(card.more).toBe(1)
    expect(card.routed).toEqual(['verdict = below', 'size = high'])
    expect(card.shadow).toBe(false)
  })

  it('marks shadow evidence and keeps native and emulation apart', () => {
    const card = judgeCard(
      judge({
        decided_by: 'emulated:local',
        evidence: [
          { seq: 3, source: 'native', mode: 'shadow', applied: false, provider: 'typesafe', model: 'jev', error: null },
          { seq: 4, source: 'emulation', mode: 'enforce', applied: false, provider: 'local', model: null, error: null },
        ],
      }),
    )
    expect(card.shadow).toBe(true)
    expect(card.lines).toEqual(['decided by emulated:local'])
    expect(card.title).toContain('native: shadow, not applied, typesafe/jev')
    expect(card.title).toContain('emulation: enforce, not applied, local')
  })

  it('keeps the latest execution per node', () => {
    const m = latestJudges([judge({ execution: 2, decided_by: 'b' }), judge({ execution: 1, decided_by: 'a' })])
    expect(m.get('j')?.decided_by).toBe('b')
  })
})

describe('overlay projection', () => {
  it('projects each segment through its own mapping and never an unknown one', () => {
    const map = (segment: number, rows: [EdgeAddress | null, EdgeAddress | null][] | null, missing: string[] = []): SegmentMapping => ({
      segment,
      correspondence: rows ? rows.map(([before, after]) => ({ before, after, changed: [] })) : null,
      missing_in_shown: missing,
      missing_in_run: [],
    })
    const g = graph({
      segments: [seg(0), seg(1), seg(2, false)],
      edges: [
        // Segment 0 knew the edge as `a>b#2`, the shown version as `id:go`.
        edge(0, d('a', 'b', 2), { state: 'taken', count: 1 } as never),
        // Segment 1 renamed it again; the same shown edge, taken once more.
        edge(1, { kind: 'authored', id: 'old' }, { state: 'taken', count: 1 } as never),
        // Segment 1's `a>c#1` matches the shown `a>c#1`, not taken.
        edge(1, d('a', 'c'), { state: 'not_taken' } as never),
      ],
      traversals: [
        { seq: 30, from: 'a', to: 'z', execution: 1, via_policy: false, policy: null, counted: true, edges: [], segment: 2, resolved: false, reason: 'definition unknown' },
      ],
    })
    const p = projectOverlay(
      {
        mapping: [
          map(0, [[d('a', 'b', 2), { kind: 'authored', id: 'go' }]], ['gone']),
          map(1, [
            [{ kind: 'authored', id: 'old' }, { kind: 'authored', id: 'go' }],
            [d('a', 'c'), d('a', 'c')],
          ]),
          map(2, null),
        ],
      },
      g,
    )
    expect(p.edges.get('id:go')).toMatchObject({ state: 'taken', count: 2 })
    expect(p.edges.get('e:a>c#1')?.state).toBe('not_taken')
    expect(p.edges.has('e:a>b#2')).toBe(false)
    expect(p.unprojected).toEqual([{ segment: 2, text: 'not established: a -> z (definition unknown)' }])
    expect(p.missingInShown).toEqual(['gone'])
  })

  it('banners a non-exact run and nodes missing in the shown version', () => {
    expect(overlayBanner({ chosen: 'r1', alignment: 'exact', reason: null }, [])).toBeNull()
    expect(overlayBanner({ chosen: 'r1', alignment: 'patched', reason: 'a supervisor patch' }, ['x'])).toBe(
      'Run r1 is patched: a supervisor patch. Nodes the run had and this version lacks: x.',
    )
    expect(alignmentText('same_version_other_definition')).toBe('same version, other definition')
  })

  it('sums the deliveries of each segment: a resume over the same bytes and a patch away and back', () => {
    const a = d('gate', 'discard', 2)
    // A resume: segment 1 runs the bytes of segment 0 again.
    const resumed = graph({
      segments: [seg(0), { ...seg(1), digest: 'd0', opened_by: 'resume' }],
      shown_segment: 1,
      edges: [edge(0, a, { state: 'taken', count: 3 } as never), edge(1, a, { state: 'not_taken' } as never)],
    })
    expect(runEdgeLooks(resumed).get(r(a))?.count).toBe(3)
    // DUP, patched to A, patched back to DUP: segments 0 and 2 each took
    // the edge once; the canvas of the shown DUP and the overlay say 2.
    const aba = graph({
      segments: [seg(0), seg(1), { ...seg(2), digest: 'd0' }],
      shown_segment: 2,
      edges: [
        edge(0, a, { state: 'taken', count: 1 } as never),
        edge(1, d('gate', 'discard'), { state: 'taken', count: 1 } as never),
        edge(2, a, { state: 'taken', count: 1 } as never),
      ],
    })
    expect(runEdgeLooks(aba).get(r(a))?.count).toBe(2)
    const same = (seg: number, rows: [EdgeAddress, EdgeAddress][]): SegmentMapping => ({
      segment: seg,
      correspondence: rows.map(([before, after]) => ({ before, after, changed: [] })),
      missing_in_shown: [],
      missing_in_run: [],
    })
    const p = projectOverlay(
      { mapping: [same(0, [[a, a]]), same(1, [[d('gate', 'discard'), d('gate', 'discard')]]), same(2, [[a, a]])] },
      aba,
    )
    expect(p.edges.get(r(a))?.count).toBe(2)
    expect(p.edges.get(r(d('gate', 'discard')))?.state).toBe('taken')
  })
})
