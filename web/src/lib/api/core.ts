// Playbooks, runs, profiles and the small read-only lookups (agents, models,
// skills, projects): one function per endpoint, no UI state.

import type {
  Project,
  RunDetail,
  RunSummary,
  VersionDiff,
  VersionInfo,
  PlaybookDetail,
  PlaybookSummary,
  WriteResult,
} from '../types'
import { getJson, jsonHeaders, pb, qs, requestJson, run } from './http'


export const fetchProjects = () => getJson<Project[]>('/api/projects')

export interface ProfileSummary {
  name: string
  scope: string
  description: string
  trusted: boolean
  agent: string
  model: string
  skills: string[]
  workspace_id: string
  project: string
}
export interface ProfileDetail {
  name: string
  scope: string
  profile_yaml: string
  soul_md: string
  profile_digest: string
}
export interface ProfileWriteBody {
  name: string
  scope: string
  agent: string
  model: string
  // Ordered executor fallbacks, walked top to bottom when a step fails. The
  // primary pair stays in `agent`/`model`; an empty array means no fallbacks.
  fallbacks?: { agent: string; model: string }[]
  description?: string
  soul?: string
  skills?: string[]
  soul_requirement?: string
  expected_digest?: string | null
}

export const fetchProfiles = () =>
  getJson<{ profiles: ProfileSummary[] }>('/api/profiles').then((r) => r.profiles)

export const fetchProfile = (name: string, scope: string, workspace = '') =>
  getJson<ProfileDetail>(`/api/profiles/${encodeURIComponent(name)}${qs({ scope, workspace })}`)

export const writeProfile = (body: ProfileWriteBody, workspace = '') =>
  requestJson<{ name: string }>(`/api/profiles${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  })

export const deleteProfile = (name: string, scope: string, workspace = '', force = false) =>
  requestJson<{ deleted: boolean }>(
    `/api/profiles/${encodeURIComponent(name)}${qs({ scope, workspace, force: force ? 'true' : '' })}`,
    { method: 'DELETE' },
  )

export interface AgentInfo {
  agent: string
  installed: boolean
  version?: string | null
  category?: string
  models?: { items: string[]; authority: string } | null
}
export const fetchAgents = () =>
  getJson<{ agents: AgentInfo[] }>('/api/agents').then((r) => r.agents)

export interface ModelRow {
  id: string
  vendor: string
  reasoning?: string | null
}

// One model choice offered for a specific agent (issue #42 finding 9): the
// curated table filtered to that agent's vendor (or the whole table for an
// aggregator), annotated `detected` when the agent's local config/detected
// model list also names it. Detection only annotates or extends this list,
// it never replaces it - see `apb_core::models_table::model_options_for_agent`.
export interface ModelOption {
  id: string
  vendor: string
  detected: boolean
}
export const fetchModels = () =>
  getJson<{
    models: ModelRow[]
    claude_static: string[]
    options_by_agent: Record<string, ModelOption[]>
  }>('/api/models')

export interface AvailableSkill {
  name: string
  scope: string
}
export const fetchSkills = (scope: string, workspace = '') =>
  getJson<{ skills: AvailableSkill[] }>(`/api/skills${qs({ scope, workspace })}`).then(
    (r) => r.skills,
  )

export const fetchPlaybooks = () => getJson<PlaybookSummary[]>('/api/playbooks')
export const fetchPlaybook = (id: string, workspace = '', version?: string) =>
  getJson<PlaybookDetail>(`${pb(id)}${qs({ workspace, version })}`)

export const fetchInputDraft = (id: string, workspace = '') =>
  getJson<{ instruction: string | null }>(`${pb(id)}/input-draft${qs({ workspace })}`)

export const saveInputDraft = (id: string, instruction: string, workspace = '') =>
  requestJson<{ instruction: string | null }>(`${pb(id)}/input-draft${qs({ workspace })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ instruction }),
  })

export const fetchRuns = () => getJson<RunSummary[]>('/api/runs')
export const fetchRun = (id: string, workspace = '') =>
  getJson<RunDetail>(`${run(id)}${qs({ workspace })}`)
export const fetchRunReport = (id: string, workspace = '') =>
  getJson<{ report: string }>(`${run(id)}/report${qs({ workspace })}`)

export const postReview = (
  id: string,
  node: string,
  decision: string,
  note = '',
  workspace = '',
) =>
  requestJson<{ posted_seq: number }>(`${run(id)}/review${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ node, decision, note }),
  })

// POST /api/runs/{id}/answer: the web facade for answering an interactive
// agent_task node's pending question (spec 2026-07-20-interactive-nodes).
// `node` is omitted when the run has exactly one pending question; the
// server resolves it the same way `apb_engine::post_answer` does. Always
// posted as answered_by "human" server-side - the dashboard never sends that
// field.
export const postAnswer = (
  id: string,
  body: { node?: string; answer: string },
  workspace = '',
) =>
  requestJson<{ posted_seq: number }>(`${run(id)}/answer${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(body),
  })

export const createPlaybook = (id: string, yaml: string, workspace = '') =>
  requestJson<WriteResult>(`/api/playbooks${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ id, yaml }),
  })

export const updatePlaybook = (id: string, yaml: string, workspace = '') =>
  requestJson<WriteResult>(`${pb(id)}${qs({ workspace })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ yaml }),
  })

export const deletePlaybook = (id: string, workspace = '') =>
  requestJson<{ trashed: string }>(`${pb(id)}${qs({ workspace })}`, {
    method: 'DELETE',
  })

export const setFrozen = (id: string, frozen: boolean, workspace = '') =>
  requestJson<{ id: string; frozen: boolean }>(`${pb(id)}/frozen${qs({ workspace })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ frozen }),
  })

export const saveLayout = (id: string, version: string, layout: unknown, workspace = '') =>
  requestJson<void>(`${pb(id)}/layout${qs({ version, workspace })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ layout }),
  })

export const fetchDiff = (id: string, from: string, to: string, workspace = '') =>
  getJson<VersionDiff>(`${pb(id)}/diff${qs({ from, to, workspace })}`)

export const fetchVersions = (id: string, workspace = '') =>
  getJson<VersionInfo[]>(`${pb(id)}/versions${qs({ workspace })}`)

export const runPlaybook = (id: string, workspace = '') =>
  requestJson<{ run_id: string }>(`${pb(id)}/run${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({}),
  })

export const promoteVersion = (id: string, version: string, workspace = '') =>
  requestJson<{ promoted: string }>(
    `${pb(id)}/versions/${encodeURIComponent(version)}/promote${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({}) },
  )

// Connectors (design doc section 9). The server wire shape is snake_case; the
