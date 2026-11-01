import { describe, expect, it } from 'vitest'
import type { Document } from 'yaml'
import { edgeAddresses } from './edgeaddress'
import { toFlow } from './graph'
import { issueLine, refusedIssues, removeEdge, updateNode } from './playbookedit'
import { docToString, parseDoc, parsePlaybook } from './playbookyaml'

// The editor's path for an edge, at the level of its pure functions: the
// YAML becomes the canvas model, the canvas names an edge by its address, a
// mutation resolves that address on the YAML being edited, and the result is
// written back and read again.
const YAML = `schema: 2
id: triage
name: Triage
nodes:
  - { id: start, type: start }
  - { id: gate, type: agent_task, prompt: Decide }
  - { id: discard, type: finish, outcome: success }
  - { id: keep, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  # dropped on a bad verdict
  - from: gate
    to: discard
    condition: { type: output_field, node: gate, field: verdict, equals: drop }
  - { id: keep-it, from: gate, to: keep, x_review: later, max_traversals: 2 }
  # anything else is dropped too
  - { from: gate, to: discard, fallback: true }
`

function canvas(text: string) {
  const { model, error } = parsePlaybook(text)
  if (!model) throw new Error(error)
  return { model, flow: toFlow(model, null) }
}

function edit(text: string, fn: (d: Document) => Document): string {
  const { doc, error } = parseDoc(text)
  if (!doc) throw new Error(error)
  return docToString(fn(doc))
}

describe('editing edges by address', () => {
  // What the canvas hands the editor for a clicked edge, and what a Delete
  // then does with it.
  function deleteClicked(text: string, pick: (e: ReturnType<typeof canvas>['flow']['edges'][number]) => boolean) {
    const { flow } = canvas(text)
    const clicked = flow.edges.find(pick)!
    const { address, index } = clicked.data
    return { address, after: edit(text, (d) => removeEdge(d, address!, index)) }
  }

  it('deletes the aliased edge the canvas shows, not the fallback after it', () => {
    const ALIASED = `schema: 2
id: p
nodes:
  - { id: a, type: agent_task, prompt: x }
  - { id: b, type: finish, outcome: success }
edges:
  - &cond { from: a, to: b, condition: { type: node_status, node: a, equals: success } }
  - *cond
  - { from: a, to: b, fallback: true }
`
    const { flow } = canvas(ALIASED)
    expect(flow.edges.map((e) => e.data.address)).toEqual(['e:a>b#1', 'e:a>b#2', 'e:a>b#3'])
    const { address, after } = deleteClicked(ALIASED, (e) => e.data.index === 1)
    expect(address).toBe('e:a>b#2')
    const { model } = canvas(after)
    expect(model.edges).toHaveLength(2)
    expect(model.edges[0].condition).toMatchObject({ equals: 'success' })
    expect(model.edges[1].fallback).toBe(true)
  })

  it('deletes an anchored edge and keeps what its alias means', () => {
    const ALIASED = `schema: 2
id: p
nodes:
  - { id: a, type: agent_task, prompt: x }
  - { id: b, type: finish, outcome: success }
  - { id: c, type: finish, outcome: success }
edges:
  - &cond { from: a, to: b, condition: { type: node_status, node: a, equals: success } }
  - { from: a, to: c, fallback: true }
  - *cond
`
    const { after } = deleteClicked(ALIASED, (e) => e.data.index === 0)
    const { model } = canvas(after)
    expect(model.edges).toEqual([
      { from: 'a', to: 'c', fallback: true },
      { from: 'a', to: 'b', condition: { type: 'node_status', node: 'a', equals: 'success' } },
    ])
  })

  it('deletes the clicked one of two edges sharing an authored id', () => {
    const TWICE = `schema: 2
id: p
nodes:
  - { id: a, type: agent_task, prompt: x }
  - { id: b, type: finish, outcome: success }
  - { id: c, type: finish, outcome: success }
edges:
  - { id: x, from: a, to: b }
  - { id: x, from: a, to: c }
`
    const { address, after } = deleteClicked(TWICE, (e) => e.target === 'c')
    expect(address).toBe('id:x')
    expect(canvas(after).model.edges).toEqual([{ id: 'x', from: 'a', to: 'b' }])
  })

  it('deletes nothing when the YAML changed under the selection', () => {
    const PAIR = `schema: 2
id: p
nodes:
  - { id: a, type: agent_task, prompt: x }
  - { id: b, type: finish, outcome: success }
edges:
  - { from: a, to: b, x_note: first }
  - { from: a, to: b, x_note: second }
`
    const { flow } = canvas(PAIR)
    const first = flow.edges[0].data
    expect(first.address).toBe('e:a>b#1')
    // The first edge loses its \`to\` in a hand edit: the second is now #1.
    const edited = PAIR.replace('{ from: a, to: b, x_note: first }', '{ from: a, x_note: first }')
    expect(edit(edited, (d) => removeEdge(d, first.address!, first.index))).toBe(edit(edited, (d) => d))
  })

  it('keeps an authored id and unknown keys through a node form edit', () => {
    const after = edit(YAML, (d) => updateNode(d, 'gate', { prompt: 'Decide again', title: 'Gate' }))
    const { model, flow } = canvas(after)
    const keep = model.edges.find((e) => e.id === 'keep-it')
    expect(keep).toEqual({ id: 'keep-it', from: 'gate', to: 'keep', x_review: 'later', max_traversals: 2 })
    expect(flow.edges.find((e) => e.target === 'keep')?.id).toBe('id:keep-it')
    expect(after).toContain('# anything else is dropped too')
  })
})

describe('refused save issues', () => {
  it('reads the issues of a validation refusal with their edges', () => {
    const issues = refusedIssues({
      error: 'validation',
      codes: ['V12'],
      issues: [{ code: 'V12', severity: 'error', message: 'two fallbacks', node: 'gate', edges: ['e:gate>a#1', 'id:b'] }],
    })
    expect(issues).toHaveLength(1)
    expect(issueLine(issues[0])).toBe('V12: two fallbacks (node gate; edges e:gate>a#1, id:b)')
  })

  it('reads no issues from an older refusal or another error', () => {
    expect(refusedIssues({ error: 'validation', codes: ['V12'] })).toEqual([])
    expect(refusedIssues({ error: 'frozen' })).toEqual([])
    expect(refusedIssues(undefined)).toEqual([])
  })
})
