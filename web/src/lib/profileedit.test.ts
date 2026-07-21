import { describe, expect, it } from 'vitest'
import {
  defaultAgentId,
  defaultPrimaryPair,
  disabledModelIds,
  findDuplicatePair,
  firstModelForAgent,
  firstSelectableModelForAgent,
  modelIdsForAgent,
  nextFallbackPair,
  parseProfileDoc,
} from './profileedit'
import type { AgentInfo, ModelRow } from './api'

const agents: AgentInfo[] = [
  {
    agent: 'claude',
    installed: true,
    models: {
      items: ['claude-opus-4-8', 'claude-sonnet-5', 'claude-haiku-4-5-20251001'],
      authority: 'detected',
    },
  },
  {
    agent: 'opencode',
    installed: true,
    models: { items: ['opencode/big-pickle', 'opencode/claude-fable-5'], authority: 'detected' },
  },
  { agent: 'bare-agent', installed: true, models: null },
]
const modelsTable: ModelRow[] = [{ id: 'fallback-model', vendor: 'x' }]

describe('firstModelForAgent (agent-change reset)', () => {
  it('resets to the first option of the newly selected agent', () => {
    expect(firstModelForAgent('claude', agents, modelsTable)).toBe('claude-opus-4-8')
    expect(firstModelForAgent('opencode', agents, modelsTable)).toBe('opencode/big-pickle')
  })

  it('clears to an empty string when the new agent has no models and no fallback table', () => {
    expect(firstModelForAgent('bare-agent', agents, [])).toBe('')
  })

  it('agrees with modelIdsForAgent so the reset and the visible list can never disagree', () => {
    const ids = modelIdsForAgent('claude', agents, modelsTable)
    expect(firstModelForAgent('claude', agents, modelsTable)).toBe(ids[0])
  })
})

describe('disabledModelIds (duplicate prevention by disabling, not hiding)', () => {
  it('disables only the model another group already took on the SAME agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
    ]
    // Group 1 may not reuse group 0's model, but every other claude model
    // stays selectable - and the full list is still rendered.
    expect(disabledModelIds(1, groups)).toEqual(['claude-opus-4-8'])
    expect(modelIdsForAgent('claude', agents, modelsTable)).toHaveLength(3)
  })

  it('disables nothing when the other group is on a different agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
    ]
    expect(disabledModelIds(1, groups)).toEqual([])
  })

  it('never disables a group against itself', () => {
    const groups = [{ agent: 'claude', model: 'claude-opus-4-8' }]
    expect(disabledModelIds(0, groups)).toEqual([])
  })

  it('with index -1 treats every group as taken (used when adding a row)', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
    ]
    expect(new Set(disabledModelIds(-1, groups, 'claude'))).toEqual(
      new Set(['claude-opus-4-8', 'claude-sonnet-5']),
    )
  })
})

describe('firstSelectableModelForAgent (agent-change reset per group)', () => {
  it('skips the models another group already paired with that agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
    ]
    // Group 1 switches to claude: the plain first model is taken, so the reset
    // must land on the next free one instead of recreating the pair.
    expect(firstModelForAgent('claude', agents, modelsTable)).toBe('claude-opus-4-8')
    expect(firstSelectableModelForAgent('claude', 1, groups, agents, modelsTable)).toBe(
      'claude-sonnet-5',
    )
  })

  it('clears to an empty string when every model of the new agent is taken', () => {
    const groups = [
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5' },
      { agent: 'claude', model: 'claude-opus-4-8' },
    ]
    expect(firstSelectableModelForAgent('opencode', 2, groups, agents, modelsTable)).toBe('')
  })
})

describe('nextFallbackPair (default for a newly added fallback)', () => {
  it('picks the first agent and model that would not duplicate an existing pair', () => {
    expect(nextFallbackPair([{ agent: 'claude', model: 'claude-opus-4-8' }], agents, modelsTable))
      .toEqual({ agent: 'claude', model: 'claude-sonnet-5' })
  })

  it('moves on to the next agent once the obvious first choice is exhausted', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
    ]
    expect(nextFallbackPair(groups, agents, modelsTable)).toEqual({
      agent: 'opencode',
      model: 'opencode/big-pickle',
    })
  })

  it('returns null when every combination is already used, so no row is appended', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5' },
      { agent: 'bare-agent', model: 'fallback-model' },
    ]
    expect(nextFallbackPair(groups, agents, modelsTable)).toBeNull()
  })
})

describe('nextFallbackPair before the metadata arrives', () => {
  it('returns null while the agent list is still empty, which the UI must guard', () => {
    // `agents` starts empty and is filled only when GET /api/agents resolves
    // (live detection, seconds on a cold server). During that window there is
    // no pair to offer, so the Add-fallback action stays disabled instead of
    // reporting a bogus "nothing left to add".
    expect(nextFallbackPair([{ agent: 'claude', model: 'claude-opus-4-8' }], [], [])).toBeNull()
    expect(nextFallbackPair([], [], modelsTable)).toBeNull()
  })
})

describe('defaultPrimaryPair (new-profile pre-fill)', () => {
  it('picks the default agent and its first model', () => {
    expect(defaultPrimaryPair([{ agent: '', model: '' }], agents, modelsTable)).toEqual({
      agent: 'claude',
      model: 'claude-opus-4-8',
    })
  })

  it('prefers claude, else the first installed agent, else the literal claude', () => {
    expect(defaultAgentId(agents)).toBe('claude')
    expect(defaultAgentId([{ agent: 'opencode', installed: true, models: null }])).toBe('opencode')
    expect(defaultAgentId([{ agent: 'opencode', installed: false, models: null }])).toBe('claude')
    expect(defaultAgentId([])).toBe('claude')
  })

  it('does not collide with a group that already holds the obvious first model', () => {
    const groups = [
      { agent: '', model: '' },
      { agent: 'claude', model: 'claude-opus-4-8' },
    ]
    expect(defaultPrimaryPair(groups, agents, modelsTable)).toEqual({
      agent: 'claude',
      model: 'claude-sonnet-5',
    })
  })

  it('leaves the model empty when the default agent has no model left', () => {
    const groups = [
      { agent: '', model: '' },
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
    ]
    expect(defaultPrimaryPair(groups, agents, modelsTable).model).toBe('')
  })
})

describe('findDuplicatePair (save validation)', () => {
  it('accepts a unique chain', () => {
    expect(
      findDuplicatePair([
        { agent: 'claude', model: 'claude-opus-4-8' },
        { agent: 'claude', model: 'claude-sonnet-5' },
        { agent: 'opencode', model: 'opencode/big-pickle' },
      ]),
    ).toBeNull()
  })

  it('reports the repeated pair, including one produced by a custom typed value', () => {
    // The combobox disables the taken row, but a hand-typed value bypasses
    // that - save must still refuse it. Surrounding spaces do not launder it.
    expect(
      findDuplicatePair([
        { agent: 'claude', model: 'claude-opus-4-8' },
        { agent: 'claude', model: 'claude-sonnet-5' },
        { agent: ' claude ', model: ' claude-opus-4-8 ' },
      ]),
    ).toEqual({ agent: 'claude', model: 'claude-opus-4-8' })
  })
})

describe('parseProfileDoc (anti-clobber on load)', () => {
  it('keeps the saved model verbatim even when it is not the first option for its agent', () => {
    const yamlText = `
description: test profile
executor:
  agent: claude
  model: claude-haiku-4-5-20251001
skills: []
`
    const parsed = parseProfileDoc(yamlText)
    expect(parsed.agent).toBe('claude')
    expect(parsed.model).toBe('claude-haiku-4-5-20251001')
    // Sanity: the saved model is deliberately NOT the first option for its
    // agent - exactly the case a naive agent-watching effect would clobber.
    // parseProfileDoc must not recompute it via firstModelForAgent.
    expect(firstModelForAgent(parsed.agent, agents, modelsTable)).not.toBe(parsed.model)
  })

  it('reports no fallbacks for a profile that has none', () => {
    const parsed = parseProfileDoc('executor:\n  agent: claude\n  model: claude-opus-4-8\n')
    expect(parsed.fallbacks).toEqual([])
  })

  it('round-trips executor.fallbacks in order, keeping each model verbatim', () => {
    const yamlText = `
description: chained profile
executor:
  agent: claude
  model: claude-opus-4-8
  fallbacks:
    - agent: claude
      model: claude-haiku-4-5-20251001
    - agent: opencode
      model: opencode/claude-fable-5
skills: []
`
    const parsed = parseProfileDoc(yamlText)
    expect(parsed.fallbacks).toEqual([
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
      { agent: 'opencode', model: 'opencode/claude-fable-5' },
    ])
    // Both saved fallback models are deliberately NOT the first option for
    // their agent: loading must not run the agent-change reset over them.
    for (const f of parsed.fallbacks) {
      expect(firstModelForAgent(f.agent, agents, modelsTable)).not.toBe(f.model)
    }
  })
})

describe('vendor agents without an enumerated model list', () => {
  const table: ModelRow[] = [
    { id: 'claude-opus-4-8', vendor: 'anthropic' },
    { id: 'gpt-5.6-sol', vendor: 'openai' },
    { id: 'grok-4.5', vendor: 'xai' },
    { id: 'grok-4.3', vendor: 'xai' },
  ]

  it('narrows grok to the curated xAI rows', () => {
    const list: AgentInfo[] = [{ agent: 'grok', installed: true, category: 'vendor' }]
    expect(modelIdsForAgent('grok', list, table)).toEqual(['grok-4.5', 'grok-4.3'])
  })

  it('leaves an aggregator like cursor on the full table', () => {
    const list: AgentInfo[] = [{ agent: 'cursor', installed: true, category: 'aggregator' }]
    expect(modelIdsForAgent('cursor', list, table)).toEqual([
      'claude-opus-4-8',
      'gpt-5.6-sol',
      'grok-4.5',
      'grok-4.3',
    ])
  })
})
