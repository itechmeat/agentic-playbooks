import { describe, expect, it } from 'vitest'
import { parseDocument, type Document } from 'yaml'
import { addNode, updateNode } from './playbookedit'
import { parsePlaybook } from './playbookyaml'
import { edgeConditionText, edgeLabel, nodeExits } from './graph'

// A bounded hunt loop with the exit edge the loops-and-graphs change adds:
// the no_new_items exit first, then the conditional loop edge, then the
// fallback (design: "The pattern authors write"). The editor has no form for
// the condition yet, so the only thing that can go wrong with it is a save
// that drops it, which would leave the exit edge unconditional.
const LOOP_YAML = `schema: 2
id: hunt-demo
name: Hunt Demo
version: 1.0.0

nodes:
  - { id: start, type: start }
  - { id: hunt, type: agent_task, prompt: Search for new items }
  - { id: report, type: finish, outcome: success }

edges:
  # exit while the last two rounds found nothing new
  - { from: hunt, to: report, condition: { type: no_new_items, node: hunt, field: findings, key: id, rounds: 2 } }
  - { from: hunt, to: hunt, condition: { type: node_status, node: hunt, equals: success }, max_traversals: 6 }
  - { from: hunt, to: report, fallback: true }
`

const EXIT_CONDITION = {
  type: 'no_new_items',
  node: 'hunt',
  field: 'findings',
  key: 'id',
  rounds: 2,
}

function docOf(text: string): Document {
  const d = parseDocument(text)
  if (d.errors.length) throw new Error(d.errors[0].message)
  return d
}

// The condition of the first edge of `from` that carries one, from the
// re-parsed plain object (what a reload of the saved text produces).
function firstCondition(text: string, from: string): Record<string, unknown> | undefined {
  const plain = docOf(text).toJS() as {
    edges: { from: string; condition?: Record<string, unknown> }[]
  }
  return plain.edges.find((e) => e.from === from && e.condition)?.condition
}

describe('no_new_items round trip through the editor', () => {
  it('keeps the condition with all its fields on a save and reload', () => {
    const saved = docOf(LOOP_YAML).toString()
    expect(firstCondition(saved, 'hunt')).toEqual(EXIT_CONDITION)
  })

  it('keeps the condition while an unrelated node field is edited', () => {
    // What NodePanel does for every control: a patch of node keys through
    // updateNode, then the document is serialized and saved.
    const edited = updateNode(docOf(LOOP_YAML), 'hunt', { prompt: 'Look again, slower' })
    const saved = edited.toString()
    expect(saved).toContain('Look again, slower')
    expect(firstCondition(saved, 'hunt')).toEqual(EXIT_CONDITION)
  })

  it('keeps the condition while another node is added', () => {
    const saved = addNode(docOf(LOOP_YAML), 'script', 'lint').toString()
    expect(firstCondition(saved, 'hunt')).toEqual(EXIT_CONDITION)
  })

  it('keeps a condition without its optional key', () => {
    const yaml = LOOP_YAML.replace(
      'field: findings, key: id, rounds: 2',
      'field: tags, rounds: 1',
    )
    const saved = updateNode(docOf(yaml), 'report', { outcome: 'failure' }).toString()
    expect(firstCondition(saved, 'hunt')).toEqual({
      type: 'no_new_items',
      node: 'hunt',
      field: 'tags',
      rounds: 1,
    })
  })

  it('keeps the condition in the parsed model the canvas reads on load', () => {
    const { model, error } = parsePlaybook(LOOP_YAML)
    expect(error).toBeUndefined()
    expect(model?.edges[0].condition).toEqual(EXIT_CONDITION)
  })
})

describe('labels of a no_new_items exit', () => {
  const exit = {
    from: 'hunt',
    to: 'report',
    condition: { type: 'no_new_items', node: 'hunt', field: 'findings', key: 'id', rounds: 2 },
  }

  it('labels the edge with the rounds count', () => {
    expect(edgeLabel(exit)).toBe('no new items x2')
    expect(
      edgeLabel({ ...exit, condition: { type: 'no_new_items', node: 'hunt', field: 'tags', rounds: 1 } }),
    ).toBe('no new items x1')
  })

  it('captions the exit with the same label and the full meaning in the tooltip', () => {
    const pb = {
      id: 'hunt-demo',
      name: 'Hunt Demo',
      nodes: [
        { id: 'hunt', type: 'agent_task', title: 'Hunt' },
        { id: 'report', type: 'finish', title: 'Report' },
      ],
      edges: [exit],
    } as Parameters<typeof nodeExits>[0]
    const exits = nodeExits(pb, 'hunt')
    expect(exits?.list.map((e) => e.label)).toEqual(['no new items x2'])
    expect(exits?.list[0].tone).toBe('default')
    // The tooltip names the node it reads, the field, the key and the target.
    // The condition reads the edge's own source, so it says "this node".
    expect(exits?.list[0].title).toContain('successful visits of this node')
    expect(exits?.list[0].title).toContain('"findings" items keyed by their "id"')
    expect(exits?.list[0].title).toContain('goes to report')
    // A condition on another node names it.
    expect(
      edgeConditionText({
        ...exit,
        condition: { type: 'no_new_items', node: 'scout', field: 'findings', key: 'id', rounds: 2 },
      }),
    ).toContain('successful visits of node scout')
    // The edge panel shows the same text as the "taken" line.
    expect(edgeConditionText(exit)).toBe(exits?.list[0].title)
  })

  it('keeps the rounds count visible among numbered exits', () => {
    const pb = {
      id: 'hunt-demo',
      name: 'Hunt Demo',
      nodes: [
        { id: 'hunt', type: 'agent_task', title: 'Hunt' },
        { id: 'report', type: 'finish', title: 'Report' },
        { id: 'keep', type: 'finish', title: 'Keep' },
      ],
      edges: [
        exit,
        { from: 'hunt', to: 'keep', condition: { type: 'node_status', node: 'hunt', equals: 'success' } },
      ],
    } as Parameters<typeof nodeExits>[0]
    const exits = nodeExits(pb, 'hunt')
    // Two named exits are numbered, and the caption is truncated to fit; the
    // rounds number is the part that tells this exit apart, so it survives.
    expect(exits?.list[0].label).toContain('x2')
  })
})
