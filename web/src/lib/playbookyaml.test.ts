import { describe, expect, it } from 'vitest'
import { toFlow } from './graph'
import { parse } from 'yaml'
import { CURRENT_SCHEMA } from './api.gen'
import { NEW_PLAYBOOK_TEMPLATE, parsePlaybook } from './playbookyaml'

// F22: the editor's "New playbook" starts from a document in the schema this
// apb writes, not a stale literal.
describe('NEW_PLAYBOOK_TEMPLATE', () => {
  it('declares the current schema', () => {
    expect(parse(NEW_PLAYBOOK_TEMPLATE).schema).toBe(CURRENT_SCHEMA)
  })
})

// Simplified sample shaped like crates/apb-core/tests/fixtures/valid.yaml
const VALID_YAML = `schema: 1
id: implement-task
name: Implement Task
description: Example playbook

nodes:
  - id: start
    type: start
    title: Start
  - id: plan
    type: agent_task
    title: Plan
    prompt: Plan the task
  - id: done
    type: finish
    outcome: success

edges:
  - { from: start, to: plan }
  - { from: plan, to: done, condition: { type: node_status, node: plan, equals: success } }
`

describe('parsePlaybook', () => {
  it('parses valid YAML into a model suitable for toFlow', () => {
    const { model, error } = parsePlaybook(VALID_YAML)
    expect(error).toBeUndefined()
    expect(model).toBeDefined()
    expect(model!.nodes).toHaveLength(3)
    expect(model!.edges).toHaveLength(2)
    expect(() => toFlow(model!, null)).not.toThrow()
    const { nodes, edges } = toFlow(model!, null)
    expect(nodes).toHaveLength(3)
    expect(edges).toHaveLength(2)
  })

  it('returns error for broken YAML', () => {
    const { model, error } = parsePlaybook('nodes:\n  - id: [unclosed')
    expect(model).toBeUndefined()
    expect(error).toBeTruthy()
  })
})
