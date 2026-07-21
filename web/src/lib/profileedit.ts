import { parse } from 'yaml'
import type { AgentInfo, ModelOption, ModelRow } from './api'

/**
 * Model options offered for a given agent: the server-computed, curated-table-
 * driven list from `GET /api/models`'s `options_by_agent` (issue #42 finding
 * 9 - see `apb_core::models_table::model_options_for_agent`). Detection only
 * ANNOTATES a row `detected` there, it never narrows the set; a config-only
 * model absent from the curated table still arrives as its own detected-only
 * entry. Legacy `claude-code` probes as `claude`. An agent absent from
 * `optionsByAgent` (a hand-typed id detection never saw) falls back to the
 * full curated table, undetected - the same "no vendor tie, show everything"
 * behavior an aggregator gets server-side.
 */
export function modelOptionsForAgent(
  agent: string,
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): ModelOption[] {
  const probe = agent === 'claude-code' ? 'claude' : agent
  const opts = optionsByAgent[probe]
  if (opts) return opts
  return fallbackModels.map((m) => ({ id: m.id, vendor: m.vendor, detected: false }))
}

/**
 * Model ids offered for a given agent, in the same order as
 * `modelOptionsForAgent`. Used by the selection helpers below, which only
 * ever need the id.
 */
export function modelIdsForAgent(
  agent: string,
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): string[] {
  return modelOptionsForAgent(agent, optionsByAgent, fallbackModels).map((o) => o.id)
}

/**
 * The model to select right after the user switches the agent: the first
 * entry of the new agent's model list, or an empty string when it has none.
 *
 * Callers must invoke this only from a genuine user selection (e.g. a
 * Combobox `onChange`), never from a programmatic `agent` assignment such as
 * loading a saved profile - otherwise an existing profile's model is
 * silently overwritten on open.
 */
export function firstModelForAgent(
  agent: string,
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): string {
  return modelIdsForAgent(agent, optionsByAgent, fallbackModels)[0] ?? ''
}

/**
 * One entry of a profile's executor chain: the primary executor (index 0) or
 * one of its ordered fallbacks. Only the agent/model pair varies - SOUL and
 * skills are shared by the whole chain.
 */
export interface ExecutorGroup {
  agent: string
  model: string
}

const pairKey = (agent: string, model: string) => `${agent.trim()}\x00${model.trim()}`

/**
 * Model ids that group `index` must not select because another group already
 * pairs them with the same agent. Returned so the caller can DISABLE those
 * rows rather than hide them: the user should see that the option exists and
 * is taken. Only groups on the same agent contribute - switching agent frees
 * the models again.
 *
 * Pass `index = -1` to consider every group (nothing is treated as "self").
 */
export function disabledModelIds(
  index: number,
  groups: ExecutorGroup[],
  agent = groups[index]?.agent ?? '',
): string[] {
  const target = agent.trim()
  if (!target) return []
  const taken = new Set<string>()
  groups.forEach((g, i) => {
    if (i === index) return
    if (g.agent.trim() !== target) return
    if (g.model.trim()) taken.add(g.model.trim())
  })
  return [...taken]
}

/**
 * The model to select right after the user switches group `index` to `agent`:
 * the first model of that agent that is not already taken by another group.
 * Returns an empty string when the agent has no models or every one of them is
 * taken - the field then stays empty and save validation asks for a value.
 *
 * Same anti-clobber contract as `firstModelForAgent`: call it only from a
 * genuine user selection, never from a programmatic assignment.
 */
export function firstSelectableModelForAgent(
  agent: string,
  index: number,
  groups: ExecutorGroup[],
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): string {
  const taken = new Set(disabledModelIds(index, groups, agent))
  return modelIdsForAgent(agent, optionsByAgent, fallbackModels).find((id) => !taken.has(id)) ?? ''
}

/**
 * The agent/model pair a freshly added fallback should start on: the first
 * installed agent that still has a free model, with that model. `null` when
 * every installed agent/model combination is already used, in which case the
 * caller must not append a row.
 */
export function nextFallbackPair(
  groups: ExecutorGroup[],
  agents: AgentInfo[],
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): ExecutorGroup | null {
  for (const a of agents.filter((x) => x.installed)) {
    const model = firstSelectableModelForAgent(a.agent, -1, groups, optionsByAgent, fallbackModels)
    if (model) return { agent: a.agent, model }
  }
  return null
}

/**
 * The agent a brand new profile starts on: the installed `claude` agent when
 * present, otherwise the first installed one, otherwise the literal `claude`
 * so the field is never blank even when nothing is detected.
 */
export function defaultAgentId(agents: AgentInfo[]): string {
  const installed = agents.filter((x) => x.installed)
  return installed.find((a) => a.agent === 'claude')?.agent ?? installed[0]?.agent ?? 'claude'
}

/**
 * The primary agent/model pair a brand NEW profile opens on, so the form is
 * usable straight away instead of showing an empty model that only fails at
 * save time. The model is the default agent's first model that no other group
 * already holds, so pre-filling can never create a duplicate pair.
 *
 * Strictly a new-profile helper: never call it on the load path of an existing
 * profile, where a saved model must survive verbatim even when it is not the
 * first entry of its agent's list.
 */
export function defaultPrimaryPair(
  groups: ExecutorGroup[],
  agents: AgentInfo[],
  optionsByAgent: Record<string, ModelOption[]>,
  fallbackModels: ModelRow[],
): ExecutorGroup {
  const agent = defaultAgentId(agents)
  return {
    agent,
    model: firstSelectableModelForAgent(agent, 0, groups, optionsByAgent, fallbackModels),
  }
}

/**
 * The first pair that repeats an earlier one, or `null` when the chain is
 * unique. Used by save validation: the combobox allows arbitrary typed values,
 * so a duplicate can still be produced by hand and must be refused before it
 * reaches the server.
 */
export function findDuplicatePair(groups: ExecutorGroup[]): ExecutorGroup | null {
  const seen = new Set<string>()
  for (const g of groups) {
    const key = pairKey(g.agent, g.model)
    if (seen.has(key)) return { agent: g.agent.trim(), model: g.model.trim() }
    seen.add(key)
  }
  return null
}

export interface ParsedProfileDoc {
  agent: string
  model: string
  fallbacks: ExecutorGroup[]
  description: string
  skills: string[]
}

/**
 * Extracts the editable fields from a saved profile's YAML text. Every model
 * - the primary one and each fallback - is taken verbatim and is never
 * recomputed from the agent's model list, so a saved pair survives loading
 * even when its model is not the first entry for its agent. The fallback
 * order is preserved: the engine walks the chain top to bottom.
 */
export function parseProfileDoc(yamlText: string): ParsedProfileDoc {
  const doc = (parse(yamlText) ?? {}) as {
    description?: string
    executor?: { agent?: string; model?: string; fallbacks?: { agent?: string; model?: string }[] }
    skills?: (string | { name: string })[]
  }
  return {
    agent: doc.executor?.agent ?? 'claude-code',
    model: doc.executor?.model ?? '',
    fallbacks: (doc.executor?.fallbacks ?? []).map((f) => ({
      agent: f?.agent ?? '',
      model: f?.model ?? '',
    })),
    description: doc.description ?? '',
    skills: (doc.skills ?? []).map((s) => (typeof s === 'string' ? s : s.name)),
  }
}
