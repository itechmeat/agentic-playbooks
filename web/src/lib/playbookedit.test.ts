import { describe, expect, it } from 'vitest'
import { parseDocument, type Document } from 'yaml'
import {
  addEdge,
  addNode,
  removeEdge,
  removeNode,
  suggestNodeId,
  updateNode,
} from './playbookedit'

// A workflow with all top-level fields (schema, version, params, executors,
// defaults, supervisor). Structural edits must preserve them.
const FULL_YAML = `schema: 1
id: implement-task
name: Implement Task
version: 1.0.0

params:
  - { name: task, type: text }

executors:
  main:
    agent: claude-code
    model: claude-fable-5

defaults:
  executor: main
  max_retries: 1

nodes:
  - id: start
    type: start
    title: Start
  - id: plan
    type: agent_task
    title: Plan
    prompt: Plan it
  - id: done
    type: finish
    outcome: success

edges:
  - { from: start, to: plan }
  - { from: plan, to: done }
`

function docOf(text: string): Document {
  const d = parseDocument(text)
  if (d.errors.length) throw new Error(d.errors[0].message)
  return d
}

// YAMLSeq item: supports get/has by key. Document.get returns the base Node,
// so without a cast to seq there is no .items field.
type Item = { get: (k: string) => unknown; has: (k: string) => boolean }
function items(doc: Document, key: string): Item[] {
  const seq = doc.get(key) as unknown as { items: Item[] } | undefined
  return seq?.items ?? []
}

// List of top-level fields that must not be lost when editing nodes/edges.
const TOP_FIELDS = ['schema', 'id', 'name', 'version', 'params', 'executors', 'defaults']

function topKeys(doc: Document): string[] {
  const root = doc.contents as unknown as { items: { key: { value: string } }[] } | null
  return (root?.items ?? []).map((p) => p.key.value)
}

describe('addNode', () => {
  it('appends a node with given kind and id', () => {
    const doc = docOf(FULL_YAML)
    const next = addNode(doc, 'script', 'lint')
    const ns = items(next, 'nodes')
    expect(ns.map((n) => n.get('id'))).toContain('lint')
    const lint = ns.find((n) => n.get('id') === 'lint')!
    expect(lint.get('type')).toBe('script')
  })

  it('preserves all other top-level fields', () => {
    const doc = docOf(FULL_YAML)
    const next = addNode(doc, 'script', 'lint')
    const keys = topKeys(next)
    for (const f of TOP_FIELDS) expect(keys).toContain(f)
    expect(next.get('executors')).toBeDefined()
    expect(next.get('params')).toBeDefined()
  })

  it('does not mutate the input document', () => {
    const doc = docOf(FULL_YAML)
    const before = doc.toString()
    addNode(doc, 'script', 'lint')
    expect(doc.toString()).toBe(before)
  })

  it('creates nodes array if missing', () => {
    const doc = docOf('id: demo\nname: Demo\n')
    const next = addNode(doc, 'start', 'start')
    expect(items(next, 'nodes')).toHaveLength(1)
  })
})

describe('removeNode', () => {
  it('removes the node and its related edges', () => {
    const doc = docOf(FULL_YAML)
    const next = removeNode(doc, 'plan')
    const ns = items(next, 'nodes')
    expect(ns.map((n) => n.get('id'))).not.toContain('plan')
    for (const e of items(next, 'edges')) {
      expect(e.get('from')).not.toBe('plan')
      expect(e.get('to')).not.toBe('plan')
    }
  })

  it('preserves all other top-level fields', () => {
    const doc = docOf(FULL_YAML)
    const next = removeNode(doc, 'plan')
    for (const f of TOP_FIELDS) expect(topKeys(next)).toContain(f)
  })

  it('does not mutate the input document', () => {
    const doc = docOf(FULL_YAML)
    const before = doc.toString()
    removeNode(doc, 'plan')
    expect(doc.toString()).toBe(before)
  })
})

describe('updateNode', () => {
  it('merges patch into the node', () => {
    const doc = docOf(FULL_YAML)
    const next = updateNode(doc, 'plan', { prompt: 'New plan', profile: 'architect' })
    const plan = items(next, 'nodes').find((n) => n.get('id') === 'plan')!
    expect(plan.get('prompt')).toBe('New plan')
    expect(plan.get('profile')).toBe('architect')
  })

  it('deletes keys set to undefined', () => {
    const doc = docOf(FULL_YAML)
    const next = updateNode(doc, 'plan', { prompt: undefined })
    const plan = items(next, 'nodes').find((n) => n.get('id') === 'plan')!
    expect(plan.has('prompt')).toBe(false)
  })

  it('preserves all other top-level fields', () => {
    const doc = docOf(FULL_YAML)
    const next = updateNode(doc, 'plan', { prompt: 'x' })
    for (const f of TOP_FIELDS) expect(topKeys(next)).toContain(f)
  })

  it('does not mutate the input document', () => {
    const doc = docOf(FULL_YAML)
    const before = doc.toString()
    updateNode(doc, 'plan', { prompt: 'x' })
    expect(doc.toString()).toBe(before)
  })

  it('sets prompt and profile on a finish node', () => {
    const src = [
      'schema: 2',
      'id: p',
      'name: p',
      'version: 1.0.0',
      'nodes:',
      '  - { id: f, type: finish, outcome: success }',
      'edges: []',
      '',
    ].join('\n')
    const doc = parseDocument(src)
    const next = updateNode(doc, 'f', { prompt: 'compose', profile: 'writer' })
    const yaml = next.toString()
    expect(yaml).toContain('prompt: compose')
    expect(yaml).toContain('profile: writer')
  })
})

describe('addEdge', () => {
  it('appends an edge from->to', () => {
    const doc = docOf(FULL_YAML)
    const next = addEdge(doc, 'start', 'done')
    const pair = items(next, 'edges').find(
      (e) => e.get('from') === 'start' && e.get('to') === 'done',
    )
    expect(pair).toBeDefined()
  })

  it('appends an edge with condition when provided', () => {
    const doc = docOf(FULL_YAML)
    const next = addEdge(doc, 'plan', 'done', { type: 'node_status', node: 'plan', equals: 'failure' })
    const pair = items(next, 'edges').find(
      (e) => e.get('from') === 'plan' && e.get('to') === 'done' && e.has('condition'),
    )!
    expect(pair).toBeDefined()
    const cond = pair.get('condition') as { get: (k: string) => unknown }
    expect(cond.get('equals')).toBe('failure')
  })

  it('preserves all other top-level fields', () => {
    const doc = docOf(FULL_YAML)
    const next = addEdge(doc, 'start', 'done')
    for (const f of TOP_FIELDS) expect(topKeys(next)).toContain(f)
  })

  it('does not mutate the input document', () => {
    const doc = docOf(FULL_YAML)
    const before = doc.toString()
    addEdge(doc, 'start', 'done')
    expect(doc.toString()).toBe(before)
  })
})

describe('removeEdge', () => {
  it('removes the matching edge by from+to', () => {
    const doc = docOf(FULL_YAML)
    const next = removeEdge(doc, 'plan', 'done')
    const es = items(next, 'edges')
    expect(es.find((e) => e.get('from') === 'plan' && e.get('to') === 'done')).toBeUndefined()
    // the other edges are still in place
    expect(es.find((e) => e.get('from') === 'start' && e.get('to') === 'plan')).toBeDefined()
  })

  it('preserves all other top-level fields', () => {
    const doc = docOf(FULL_YAML)
    const next = removeEdge(doc, 'plan', 'done')
    for (const f of TOP_FIELDS) expect(topKeys(next)).toContain(f)
  })

  it('does not mutate the input document', () => {
    const doc = docOf(FULL_YAML)
    const before = doc.toString()
    removeEdge(doc, 'plan', 'done')
    expect(doc.toString()).toBe(before)
  })
})

describe('suggestNodeId', () => {
  it('returns kind-N for the first occurrence', () => {
    const doc = docOf(FULL_YAML)
    expect(suggestNodeId(doc, 'script')).toBe('script-1')
  })

  it('increments N to avoid collisions with existing ids', () => {
    const doc = docOf(FULL_YAML)
    const withOne = addNode(doc, 'script', 'script-1')
    expect(suggestNodeId(withOne, 'script')).toBe('script-2')
  })
})

describe('round-trip field preservation', () => {
  it('parse -> addNode -> serialize keeps every top-level field', () => {
    const doc = docOf(FULL_YAML)
    const next = addNode(removeEdge(doc, 'plan', 'done'), 'condition', 'check')
    const text = next.toString()
    const reparsed = docOf(text)
    for (const f of TOP_FIELDS) {
      expect(reparsed.get(f)).toBeDefined()
    }
    // supervisor is absent from the sample, but the field must remain if present
    const withSup = docOf(FULL_YAML + 'supervisor:\n  policy: { capabilities: [observe] }\n')
    const edited = updateNode(withSup, 'plan', { prompt: 'x' })
    expect(edited.toString()).toContain('supervisor:')
  })
})
