import { describe, expect, it } from 'vitest'
import { toFlow } from './graph'
import { parsePlaybook, serializePlaybook } from './playbookyaml'

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

describe('serializePlaybook', () => {
  it('round-trips model structure', () => {
    const { model: original } = parsePlaybook(VALID_YAML)
    expect(original).toBeDefined()
    const text = serializePlaybook(original!)
    const { model: again, error } = parsePlaybook(text)
    expect(error).toBeUndefined()
    expect(again).toEqual(original)
  })
})
