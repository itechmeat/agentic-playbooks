import { parse } from 'yaml'
import type { AgentInfo, ModelRow } from './api'

/**
 * Vendor tie for a built-in agent that runs a single provider's models. Used
 * only to narrow the fallback model list of a Vendor agent that reports no
 * enumerated models of its own (grok is the current case). An aggregator has
 * no entry here and keeps the full table.
 */
const AGENT_VENDOR: Record<string, string> = {
  claude: 'anthropic',
  codex: 'openai',
  grok: 'xai',
}

/**
 * Model ids offered for a given agent: prefer that agent's detected model
 * list; otherwise fall back to the curated table. Legacy `claude-code` probes
 * as `claude`. For a Vendor agent with no enumerated list (for example an
 * unauthenticated grok), the fallback is narrowed to that vendor's rows so the
 * user is not offered models the agent cannot run; an aggregator keeps the
 * whole table. Shared by the model-options list and the agent-change reset
 * below so the two can never disagree.
 */
export function modelIdsForAgent(
  agent: string,
  agents: AgentInfo[],
  modelsTable: ModelRow[],
): string[] {
  const probe = agent === 'claude-code' ? 'claude' : agent
  const a = agents.find((x) => x.agent === probe)
  if (a?.models?.items?.length) return a.models.items
  const vendor = AGENT_VENDOR[probe]
  if (vendor && a?.category === 'vendor') {
    const rows = modelsTable.filter((m) => m.vendor === vendor)
    if (rows.length) return rows.map((m) => m.id)
  }
  return modelsTable.map((m) => m.id)
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
  agents: AgentInfo[],
  modelsTable: ModelRow[],
): string {
  return modelIdsForAgent(agent, agents, modelsTable)[0] ?? ''
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
  agents: AgentInfo[],
  modelsTable: ModelRow[],
): string {
  const taken = new Set(disabledModelIds(index, groups, agent))
  return modelIdsForAgent(agent, agents, modelsTable).find((id) => !taken.has(id)) ?? ''
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
  modelsTable: ModelRow[],
): ExecutorGroup | null {
  for (const a of agents.filter((x) => x.installed)) {
    const model = firstSelectableModelForAgent(a.agent, -1, groups, agents, modelsTable)
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
  modelsTable: ModelRow[],
): ExecutorGroup {
  const agent = defaultAgentId(agents)
  return { agent, model: firstSelectableModelForAgent(agent, 0, groups, agents, modelsTable) }
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
