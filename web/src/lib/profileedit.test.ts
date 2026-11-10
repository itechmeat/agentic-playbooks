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
  cloudModelNote,
  cloudOptionFor,
  modelHint,
  resolvedChain,
} from './profileedit'
import type { AgentInfo, CloudCatalog, ModelOption, ModelRow, PrivacyClass } from './api'

// `optionsByAgent` mirrors the server's `/api/models` `options_by_agent`
// (issue #42 finding 9): the curated table drives the SET for each agent,
// detection only annotates `detected` or contributes a detected-only extra
// absent from the table. `claude` here is fully covered by the curated table
// (its detected static list matches it 1:1); `opencode` names its own
// provider-qualified ids that the generic cross-vendor table does not carry,
// so those arrive as detected-only extras alongside the curated fallback row.
const optionsByAgent: Record<string, ModelOption[]> = {
  claude: [
    { id: 'claude-opus-5-5', vendor: 'anthropic', detected: true },
    { id: 'claude-sonnet-5', vendor: 'anthropic', detected: true },
    { id: 'claude-haiku-4-5-20251001', vendor: 'anthropic', detected: true },
  ],
  opencode: [
    { id: 'fallback-model', vendor: 'x', detected: false },
    { id: 'opencode/big-pickle', vendor: 'x', detected: true },
    { id: 'opencode/claude-fable-5-1', vendor: 'x', detected: true },
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
    expect(firstModelForAgent('claude', optionsByAgent, modelsTable)).toBe('claude-opus-5-5')
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
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'claude', model: 'claude-sonnet-5' },
    ]
    // Group 1 may not reuse group 0's model, but every other claude model
    // stays selectable - and the full list is still rendered.
    expect(disabledModelIds(1, groups)).toEqual(['claude-opus-5-5'])
    expect(modelIdsForAgent('claude', optionsByAgent, modelsTable)).toHaveLength(3)
  })

  it('disables nothing when the other group is on a different agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'opencode', model: 'fallback-model' },
    ]
    expect(disabledModelIds(1, groups)).toEqual([])
  })

  it('never disables a group against itself', () => {
    const groups = [{ agent: 'claude', model: 'claude-opus-5-5' }]
    expect(disabledModelIds(0, groups)).toEqual([])
  })

  it('with index -1 treats every group as taken (used when adding a row)', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'claude', model: 'claude-sonnet-5' },
    ]
    expect(new Set(disabledModelIds(-1, groups, 'claude'))).toEqual(
      new Set(['claude-opus-5-5', 'claude-sonnet-5']),
    )
  })

  it('reserves a claude-code group model for a claude group (legacy alias)', () => {
    const groups = [
      { agent: 'claude-code', model: 'claude-opus-5-5' },
      { agent: 'claude', model: 'claude-sonnet-5' },
    ]
    expect(disabledModelIds(1, groups)).toEqual(['claude-opus-5-5'])
  })

  it('reserves a claude group model for a claude-code group (legacy alias)', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'claude-code', model: 'claude-sonnet-5' },
    ]
    expect(disabledModelIds(1, groups)).toEqual(['claude-opus-5-5'])
  })
})

describe('firstSelectableModelForAgent (agent-change reset per group)', () => {
  it('skips the models another group already paired with that agent', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'opencode', model: 'fallback-model' },
    ]
    // Group 1 switches to claude: the plain first model is taken, so the reset
    // must land on the next free one instead of recreating the pair.
    expect(firstModelForAgent('claude', optionsByAgent, modelsTable)).toBe('claude-opus-5-5')
    expect(firstSelectableModelForAgent('claude', 1, groups, optionsByAgent, modelsTable)).toBe(
      'claude-sonnet-5',
    )
  })

  it('clears to an empty string when every model of the new agent is taken', () => {
    const groups = [
      { agent: 'opencode', model: 'fallback-model' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5-1' },
      { agent: 'claude', model: 'claude-opus-5-5' },
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
        [{ agent: 'claude', model: 'claude-opus-5-5' }],
        agents,
        optionsByAgent,
        modelsTable,
      ),
    ).toEqual({ agent: 'claude', model: 'claude-sonnet-5' })
  })

  it('moves on to the next agent once the obvious first choice is exhausted', () => {
    const groups = [
      { agent: 'claude', model: 'claude-opus-5-5' },
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
      { agent: 'claude', model: 'claude-opus-5-5' },
      { agent: 'claude', model: 'claude-sonnet-5' },
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
      { agent: 'opencode', model: 'fallback-model' },
      { agent: 'opencode', model: 'opencode/big-pickle' },
      { agent: 'opencode', model: 'opencode/claude-fable-5-1' },
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
      nextFallbackPair([{ agent: 'claude', model: 'claude-opus-5-5' }], [], {}, []),
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
      model: 'claude-opus-5-5',
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
      { agent: 'claude', model: 'claude-opus-5-5' },
    ]
    expect(defaultPrimaryPair(groups, agents, optionsByAgent, modelsTable)).toEqual({
      agent: 'claude',
      model: 'claude-sonnet-5',
    })
  })

  it('leaves the model empty when the default agent has no model left', () => {
    const groups = [
      { agent: '', model: '' },
      { agent: 'claude', model: 'claude-opus-5-5' },
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
        { agent: 'claude', model: 'claude-opus-5-5' },
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
        { agent: 'claude', model: 'claude-opus-5-5' },
        { agent: 'claude', model: 'claude-sonnet-5' },
        { agent: ' claude ', model: ' claude-opus-5-5 ' },
      ]),
    ).toEqual({ agent: 'claude', model: 'claude-opus-5-5' })
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
    const parsed = parseProfileDoc('executor:\n  agent: claude\n  model: claude-opus-5-5\n')
    expect(parsed.fallbacks).toEqual([])
  })

  it('round-trips executor.fallbacks in order, keeping each model verbatim', () => {
    const yamlText = `
description: chained profile
executor:
  agent: claude
  model: claude-opus-5-5
  fallbacks:
    - agent: claude
      model: claude-haiku-4-5-20251001
    - agent: opencode
      model: opencode/claude-fable-5-1
skills: []
`
    const parsed = parseProfileDoc(yamlText)
    expect(parsed.fallbacks).toEqual([
      { agent: 'claude', model: 'claude-haiku-4-5-20251001' },
      { agent: 'opencode', model: 'opencode/claude-fable-5-1' },
    ])
    // Both saved fallback models are deliberately NOT the first option for
    // their agent: loading must not run the agent-change reset over them.
    for (const f of parsed.fallbacks) {
      expect(firstModelForAgent(f.agent, optionsByAgent, modelsTable)).not.toBe(f.model)
    }
  })
})

describe('parseProfileDoc tiers (read-only display)', () => {
  it('has no tiers for a profile without them', () => {
    expect(parseProfileDoc('executor: { agent: claude, model: sonnet }\n').tiers).toEqual([])
  })

  it('lists the declared tiers in order, the executor tier named as such', () => {
    const yaml =
      'executor: { agent: claude, model: sonnet }\ntiers:\n  light: { agent: opencode, model: small, for: "Mechanical edits." }\n  standard: { use: executor, for: "Ordinary tasks." }\n'
    expect(parseProfileDoc(yaml).tiers).toEqual([
      { name: 'light', agent: 'opencode', model: 'small', executor: 'opencode / small', for: 'Mechanical edits.' },
      { name: 'standard', executor: 'the profile executor', for: 'Ordinary tasks.' },
    ])
  })
})

describe('cloud catalog models', () => {
  const near = (id: string, privacy_class: PrivacyClass): ModelOption => ({
    id: `nearai/${id}`,
    vendor: 'near-ai-cloud',
    detected: false,
    cloud: { provider: 'near-ai-cloud', privacy_class, tools: true, reasoning: true },
  })
  const optionsByAgent: Record<string, ModelOption[]> = {
    dsh: [
      { id: 'deepseek-flash', vendor: '', detected: false },
      { id: 'deepseek-v4-pro', vendor: '', detected: false },
      near('Qwen/Qwen3.8-27B', 'confidential_tee'),
      near('anthropic/claude-sonnet-5-5', 'incognito'),
    ],
    claude: [{ id: 'claude-haiku-5-5', vendor: 'anthropic', detected: true }],
  }

  it('keeps the class of a cloud option, listed after the DeepSeek models', () => {
    const opts = modelOptionsForAgent('dsh', optionsByAgent, [])
    expect(opts.map((o) => o.id).slice(0, 2)).toEqual(['deepseek-flash', 'deepseek-v4-pro'])
    const q = opts.find((o) => o.id === 'nearai/Qwen/Qwen3.8-27B')!
    expect(modelHint(q, false)).toBe('confidential tee')
    expect(modelHint(q, true)).toBe('in use')
    expect(cloudOptionFor('dsh', 'nearai/Qwen/Qwen3.8-27B@high', optionsByAgent)?.privacy_class).toBe(
      'confidential_tee',
    )
    expect(cloudOptionFor('dsh', 'deepseek-flash', optionsByAgent)).toBeNull()
  })

  it('notes a catalog model an agent step cannot run on', () => {
    const catalogs: CloudCatalog[] = [
      {
        provider: 'near-ai-cloud',
        display: 'NEAR AI Cloud',
        base_url: 'https://cloud-api.near.ai/v1',
        key_env: 'NEARAI_API_KEY',
        key_present: false,
        source: 'embedded',
        fetched_at: '2026-11-09T00:00:00Z',
        digest: 'sha256:x',
        dropped: 0,
        models: [
          {
            id: 'Qwen/Qwen3-VL-30B-A3B-Instruct',
            owned_by: 'nearai',
            privacy_class: 'confidential_tee',
            features: { tools: false, structured_outputs: true, json_mode: false, reasoning: false },
            input_modalities: ['text', 'image'],
            output_modalities: ['text'],
          },
        ],
      },
    ]
    expect(cloudModelNote('dsh', 'nearai/Qwen/Qwen3-VL-30B-A3B-Instruct', optionsByAgent, catalogs)).toMatch(
      /no tools/,
    )
    expect(cloudModelNote('dsh', 'nearai/Qwen/Qwen3.8-27B', optionsByAgent, catalogs)).toBeNull()
    expect(cloudModelNote('dsh', 'nearai/Qwen/Qwen3.8-72B', optionsByAgent, catalogs)).toMatch(
      /not in the catalog/,
    )
    expect(cloudModelNote('claude', 'claude-haiku-5-5', optionsByAgent, catalogs)).toBeNull()
  })

  it('lists executor, fallbacks and tiers and warns on a mixed chain', () => {
    const tiers = parseProfileDoc(
      'executor: { agent: dsh, model: nearai/Qwen/Qwen3.8-27B }\ntiers:\n  quick: { agent: claude, model: claude-haiku-5-5, for: x }\n  same: { use: executor, for: y }\n',
    ).tiers
    const chain = resolvedChain(
      [
        { agent: 'dsh', model: 'nearai/Qwen/Qwen3.8-27B' },
        { agent: 'dsh', model: 'nearai/anthropic/claude-sonnet-5-5' },
      ],
      tiers,
      optionsByAgent,
    )
    expect(chain.entries.map((e) => [e.label, e.privacy])).toEqual([
      ['executor', 'confidential_tee'],
      ['fallback 1', 'incognito'],
      ['tier quick', null],
    ])
    expect(chain.uniform).toBe(false)
    expect(chain.leastPrivate).toBeNull()
    expect(chain.warning).toBe(true)
    const pure = resolvedChain([{ agent: 'dsh', model: 'nearai/Qwen/Qwen3.8-27B' }], [], optionsByAgent)
    expect(pure).toMatchObject({ uniform: true, leastPrivate: 'confidential_tee', warning: false })
    const plain = resolvedChain([{ agent: 'claude', model: 'claude-haiku-5-5' }], [], optionsByAgent)
    expect(plain.warning).toBe(false)
  })
})
