import { describe, expect, it } from 'vitest'
import { parseDocument } from 'yaml'
import { toFlow } from './graph'
import { judgeDrag, judgeExits, newExitHandle, parseNewExit } from './judgeexits'
import { addJudgeExit, retargetEdge, routeUnavailable } from './playbookedit'
import { parsePlaybook } from './playbookyaml'
import { withIssues } from './issues'

// A judge with one question of each kind; `extra` goes into the judge.
const yaml = (judge: string, edges: string) => `schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - id: start
    type: start
  - id: j
    type: judge
    state: { input: x }
${judge}
  - id: a
    type: finish
    outcome: success
  - id: b
    type: finish
    outcome: success
edges:
  - { from: start, to: j }
${edges}`

const model = (text: string) => {
  const { model, error } = parsePlaybook(text)
  if (!model) throw new Error(error)
  return model
}

const CHOICE = `    questions:
      verdict:
        type: choice
        criteria: { ok: done, needs_work: fix it }
    thresholds:
      verdict: { min_confidence: 0.7, below: unclear }`

describe('judge exits per question kind', () => {
  it('choice: criteria keys in order, then the escape option', () => {
    const m = model(yaml(CHOICE, `  - { from: j, to: a, condition: { type: output_field, node: j, field: verdict, equals: ok } }`))
    const x = judgeExits(m, 'j')!
    expect(x.values.map((v) => v.value)).toEqual(['ok', 'needs_work', 'unclear'])
    expect(x.values[0].address).toBe('e:j>a#1')
    // Values without an edge are hollow, unnumbered, "not connected".
    expect(x.hollow.map((h) => h.id)).toEqual([newExitHandle('verdict', 'needs_work'), newExitHandle('verdict', 'unclear')])
    expect(x.hollow[0].title).toMatch(/^not connected/)
    expect(x.hollow[0].title).toContain('checked in edge order across questions')
    expect(x.hollow[0].title).toContain('below enforce')
  })

  it('noul: true and false', () => {
    const m = model(yaml(`    questions:\n      risky: { type: noul }`, ''))
    expect(judgeExits(m, 'j')!.values.map((v) => v.value)).toEqual(['true', 'false'])
  })

  it('score: band names, a gap note, and no exits without bands', () => {
    const banded = model(
      yaml(
        `    questions:\n      size: { type: score, levels: [s, m, l] }\n    thresholds:\n      size:\n        bands: { low: [0, 0.5], high: [1, 2] }`,
        '',
      ),
    )
    const x = judgeExits(banded, 'j')!
    expect(x.values.map((v) => v.value)).toEqual(['low', 'high'])
    expect(x.notes).toEqual(['size: values in gaps match no exit; add an else edge'])
    const bare = model(yaml(`    questions:\n      size: { type: score, levels: [s, m] }`, ''))
    expect(judgeExits(bare, 'j')).toEqual({ values: [], hollow: [], notes: ['add bands to route on size'] })
  })

  it('on_unavailable route adds the unavailable exit; default, emulate and fail add none', () => {
    const routed = model(yaml(`${CHOICE}\n    on_unavailable: { route: b }`, ''))
    const u = judgeExits(routed, 'j')!.values.at(-1)
    expect(u).toMatchObject({ field: 'decided_by', value: 'unavailable', route: 'b', address: null })
    for (const form of ['fail', 'emulate', '{ default: { verdict: ok } }']) {
      const m = model(yaml(`${CHOICE}\n    on_unavailable: ${form}`, ''))
      expect(judgeExits(m, 'j')!.values.map((v) => v.value)).toEqual(['ok', 'needs_work', 'unclear'])
    }
  })

  it('leaves other outgoing edges as ordinary exits, warned only when an issue names them', () => {
    const m = model(
      yaml(
        CHOICE,
        `  - { from: j, to: a, condition: { type: node_status, node: j, equals: failure } }\n  - { from: j, to: b, fallback: true }`,
      ),
    )
    expect(judgeExits(m, 'j')!.values.every((v) => v.address === null)).toBe(true)
    const flow = toFlow(m, { judgeExits: true })
    const judge = flow.nodes.find((n) => n.id === 'j')!
    expect(judge.data.exits?.list.map((e) => e.address)).toEqual(['e:j>a#1', 'e:j>b#1'])
    expect(judge.data.hollow?.length).toBe(3)
    const quiet = withIssues(flow.nodes, flow.edges, [])
    expect(quiet.edges.every((e) => !e.data.issue)).toBe(true)
    const warned = withIssues(flow.nodes, flow.edges, [
      { code: 'V55', severity: 'warning', message: 'm', edges: ['e:j>b#1'] },
    ])
    expect(warned.edges.find((e) => e.data.address === 'e:j>b#1')?.data.issue?.error).toBe(false)
    expect(warned.edges.find((e) => e.data.address === 'e:j>a#1')?.data.issue).toBeUndefined()
  })
})

describe('judge drags', () => {
  const text = yaml(
    `${CHOICE}\n    on_unavailable: { route: b }`,
    `  - { id: keep, from: j, to: a, condition: { type: output_field, node: j, field: verdict, equals: ok }, max_traversals: 2 }\n  - { from: j, to: b, fallback: true }`,
  )

  it('reads each handle', () => {
    const m = model(text)
    expect(judgeDrag(m, 'j', 'new:verdict=needs_work')).toEqual({ kind: 'add', field: 'verdict', value: 'needs_work' })
    expect(judgeDrag(m, 'j', 'edge:id:keep')).toEqual({ kind: 'retarget', address: 'id:keep' })
    expect(judgeDrag(m, 'j', 'new:decided_by=unavailable')).toEqual({ kind: 'unavailable' })
    expect(judgeDrag(m, 'j', 'edge:e:j>b#1')).toEqual({ kind: 'other' })
    expect(judgeDrag(m, 'start', 'plain')).toEqual({ kind: 'other' })
    expect(parseNewExit('new:a=b=c')).toEqual({ field: 'a', value: 'b=c' })
  })

  it('a hollow value adds a quoted output_field edge before the fallback', () => {
    const next = addJudgeExit(parseDocument(text), 'j', 'verdict', 'needs_work', 'a').toString()
    expect(next).toContain('equals: "needs_work"')
    const m = model(next)
    const i = m.edges.findIndex((e) => e.condition?.type === 'output_field' && e.condition.equals === 'needs_work')
    expect(m.edges[i + 1].fallback).toBe(true)
    // A second id-less edge of the pair j -> a? `keep` has an id, so none.
    expect(m.edges[i].id).toBeUndefined()
  })

  it('a connected value is retargeted, keeping its id and keys', () => {
    const doc = parseDocument(text)
    const next = model(retargetEdge(doc, 'id:keep', 'b').toString())
    const e = next.edges.find((x) => x.id === 'keep')!
    expect(e).toMatchObject({ to: 'b', max_traversals: 2, condition: { field: 'verdict', equals: 'ok' } })
    // Dropping it on its current target changes nothing.
    expect(retargetEdge(doc, 'id:keep', 'a').toString()).toBe(doc.toString())
  })

  it('a retarget that makes an id-less duplicate gives it an id', () => {
    const t = yaml(CHOICE, `  - { from: j, to: a, condition: { type: output_field, node: j, field: verdict, equals: ok } }\n  - { from: j, to: b, condition: { type: output_field, node: j, field: verdict, equals: unclear } }`)
    const next = model(retargetEdge(parseDocument(t), 'e:j>b#1', 'a').toString())
    const moved = next.edges.find((e) => e.condition?.type === 'output_field' && e.condition.equals === 'unclear')!
    expect(moved.to).toBe('a')
    expect(typeof moved.id).toBe('string')
  })

  it('unavailable sets the route and its edge together, creating a missing edge', () => {
    const once = routeUnavailable(parseDocument(text), 'j', 'a').toString()
    const created = model(once)
    const j = created.nodes.find((n) => n.id === 'j') as Record<string, unknown>
    expect(j.on_unavailable).toEqual({ route: 'a' })
    const edge = created.edges.find((e) => e.condition?.type === 'output_field' && e.condition.field === 'decided_by')!
    expect(edge).toMatchObject({ from: 'j', to: 'a', condition: { equals: 'unavailable' } })
    // With the edge there, both move again in one mutation; no second edge.
    const moved = model(routeUnavailable(parseDocument(once), 'j', 'b').toString())
    expect((moved.nodes.find((n) => n.id === 'j') as Record<string, unknown>).on_unavailable).toEqual({ route: 'b' })
    const routes = moved.edges.filter((e) => e.condition?.type === 'output_field' && e.condition.field === 'decided_by')
    expect(routes.map((e) => e.to)).toEqual(['b'])
  })

  it('keeps unknown YAML keys through every mutation', () => {
    const t = text.replace('state: { input: x }', 'state: { input: x }\n    x-note: keep me')
    for (const out of [
      addJudgeExit(parseDocument(t), 'j', 'verdict', 'unclear', 'a'),
      retargetEdge(parseDocument(t), 'id:keep', 'b'),
      routeUnavailable(parseDocument(t), 'j', 'a'),
    ]) {
      expect(out.toString()).toContain('x-note: keep me')
    }
  })
})
