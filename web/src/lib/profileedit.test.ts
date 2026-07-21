import { describe, expect, it } from 'vitest'
import {
  defaultAgentId,
  defaultPrimaryPair,
  disabledModelIds,
  findDuplicatePair,
  firstModelForAgent,
  firstSelectableModelForAgent,
  modelIdsForAgent,
  modelOptionsForAgent,
  nextFallbackPair,
  parseProfileDoc,
} from './profileedit'
import type { AgentInfo, ModelOption, ModelRow } from './api'

// `optionsByAgent` mirrors the server's `/api/models` `options_by_agent`
// (issue #42 finding 9): the curated table drives the SET for each agent,
// detection only annotates `detected` or contributes a detected-only extra
// absent from the table. `claude` here is fully covered by the curated table
// (its detected static list matches it 1:1); `opencode` names its own
// provider-qualified ids that the generic cross-vendor table does not carry,
// so those arrive as detected-only extras alongside the curated fallback row.
const optionsByAgent: Record<string, ModelOption[]> = {
  claude: [
    { id: 'claude-opus-4-8', vendor: 'anthropic', detected: true },
    { id: 'claude-sonnet-5', vendor: 'anthropic', detected: true },
    { id: 'claude-haiku-4-5-20251001', vendor: 'anthropic', detected: true },
  ],
  opencode: [
    { id: 'fallback-model', vendor: 'x', detected: false },
    { id: 'opencode/big-pickle', vendor: 'x', detected: true },
    { id: 'opencode/claude-fable-5', vendor: 'x', detected: true },
  ],
  'bare-agent': [],
}
const agents: AgentInfo[] = [
  { agent: 'claude', installed: true, models: null },
  { agent: 'opencode', installed: true, models: null },
  { agent: 'bare-agent', installed: true, models: null },
]
const modelsTable: ModelRow[] = [{ id: 'fallback-model', vendor: 'x' }]

describe('modelOptionsForAgent (issue #42 finding 9: curated table drives the set)', () => {
  it('returns the server-computed option list for a known agent verbatim', () => {
    expect(modelOptionsForAgent('claude', optionsByAgent, modelsTable)).toEqual(
      optionsByAgent.claude,
    )
  })

  it('probes legacy claude-code as claude', () => {
    expect(modelOptionsForAgent('claude-code', optionsByAgent, modelsTable)).toEqual(
      optionsByAgent.claude,
    )
  })

  it('falls back to the curated table, undetected, for an agent detection never saw', () => {
    expect(modelOptionsForAgent('a-hand-typed-agent', optionsByAgent, modelsTable)).toEqual([
      { id: 'fallback-model', vendor: 'x', detected: false },
    ])
  })

  it('keeps a config-only model present as its own detected entry, never as a replacement', () => {
    const opts = modelOptionsForAgent('opencode', optionsByAgent, modelsTable)
    expect(opts).toHaveLength(3)
    expect(opts).toContainEqual({ id: 'fallback-model', vendor: 'x', detected: false })
    expect(opts).toContainEqual({ id: 'opencode/big-pickle', vendor: 'x', detected: true })
  })
})

describe('firstModelForAgent (agent-change reset)', () => {
  it('resets to the first option of the newly selected agent', () => {
    expect(firstModelForAgent('claude', optionsByAgent, modelsTable)).toBe('claude-opus-4-8')
    expect(firstModelForAgent('opencode', optionsByAgent, modelsTable)).toBe('fallback-model')
  })

  it('clears to an empty string when the new agent has no models and no fallback table', () => {
    expect(firstModelForAgent('bare-agent', optionsByAgent, [])).toBe('')
  })

  it('agrees with modelIdsForAgent so the reset and the visible list can never disagree', () => {
    const ids = modelIdsForAgent('claude', optionsByAgent, modelsTable)
    expect(firstModelForAgent('claude', optionsByAgent, modelsTable)).toBe(ids[0])
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
    expect(modelIdsForAgent('claude', optionsByAgent, modelsTable)).toHaveLength(3)
  })

  it('disables nothing when the other group is on a different agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'opencode', model: 'fallback-model' },
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
      { agent: 'opencode', model: 'fallback-model' },
    ]
    // Group 1 switches to claude: the plain first model is taken, so the reset
    // must land on the next free one instead of recreating the pair.
    expect(firstModelForAgent('claude', optionsByAgent, modelsTable)).toBe('claude-opus-4-8')
    expect(firstSelectableModelForAgent('claude', 1, groups, optionsByAgent, modelsTable)).toBe(
      'claude-sonnet-5',
    )
  })

  it('clears to an empty string when every model of the new agent is taken', () => {
    const groups = [
      { agent: 'opencode', model: 'fallback-model' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5' },
      { agent: 'claude', model: 'claude-opus-4-8' },
    ]
    expect(
      firstSelectableModelForAgent('opencode', 3, groups, optionsByAgent, modelsTable),
    ).toBe('')
  })
})

describe('nextFallbackPair (default for a newly added fallback)', () => {
  it('picks the first agent and model that would not duplicate an existing pair', () => {
    expect(
      nextFallbackPair(
        [{ agent: 'claude', model: 'claude-opus-4-8' }],
        agents,
        optionsByAgent,
        modelsTable,
      ),
    ).toEqual({ agent: 'claude', model: 'claude-sonnet-5' })
  })

  it('moves on to the next agent once the obvious first choice is exhausted', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
    ]
    expect(nextFallbackPair(groups, agents, optionsByAgent, modelsTable)).toEqual({
      agent: 'opencode',
      model: 'fallback-model',
    })
  })

  it('returns null when every combination is already used, so no row is appended', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-4-8' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
      { agent: 'opencode', model: 'fallback-model' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5' },
      { agent: 'bare-agent', model: 'anything' },
    ]
    expect(nextFallbackPair(groups, agents, optionsByAgent, modelsTable)).toBeNull()
  })
})

describe('nextFallbackPair before the metadata arrives', () => {
  it('returns null while the agent list is still empty, which the UI must guard', () => {
    // `agents` starts empty and is filled only when GET /api/agents resolves
    // (live detection, seconds on a cold server). During that window there is
    // no pair to offer, so the Add-fallback action stays disabled instead of
    // reporting a bogus "nothing left to add".
    expect(
      nextFallbackPair([{ agent: 'claude', model: 'claude-opus-4-8' }], [], {}, []),
    ).toBeNull()
    expect(nextFallbackPair([], [], {}, modelsTable)).toBeNull()
  })
})

describe('defaultPrimaryPair (new-profile pre-fill)', () => {
  it('picks the default agent and its first model', () => {
    expect(
      defaultPrimaryPair([{ agent: '', model: '' }], agents, optionsByAgent, modelsTable),
    ).toEqual({
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
    expect(defaultPrimaryPair(groups, agents, optionsByAgent, modelsTable)).toEqual({
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
    expect(defaultPrimaryPair(groups, agents, optionsByAgent, modelsTable).model).toBe('')
  })
})

describe('findDuplicatePair (save validation)', () => {
  it('accepts a unique chain', () => {
    expect(
      findDuplicatePair([
        { agent: 'claude', model: 'claude-opus-4-8' },
        { agent: 'claude', model: 'claude-sonnet-5' },
        { agent: 'opencode', model: 'fallback-model' },
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
    expect(firstModelForAgent(parsed.agent, optionsByAgent, modelsTable)).not.toBe(parsed.model)
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
      expect(firstModelForAgent(f.agent, optionsByAgent, modelsTable)).not.toBe(f.model)
    }
  })
})

describe('vendor-tied agents (server narrows the curated table; the client only reads it)', () => {
  const table: Record<string, ModelOption[]> = {
    grok: [
      { id: 'grok-4.5', vendor: 'xai', detected: false },
      { id: 'grok-4.3', vendor: 'xai', detected: false },
    ],
    cursor: [
      { id: 'claude-opus-4-8', vendor: 'anthropic', detected: false },
      { id: 'gpt-5.6-sol', vendor: 'openai', detected: false },
      { id: 'grok-4.5', vendor: 'xai', detected: false },
      { id: 'grok-4.3', vendor: 'xai', detected: false },
    ],
  }

  it('narrows grok to the curated xAI rows', () => {
    expect(modelIdsForAgent('grok', table, [])).toEqual(['grok-4.5', 'grok-4.3'])
  })

  it('leaves an aggregator like cursor on the full table', () => {
    expect(modelIdsForAgent('cursor', table, [])).toEqual([
      'claude-opus-4-8',
      'gpt-5.6-sol',
      'grok-4.5',
      'grok-4.3',
    ])
  })
})
