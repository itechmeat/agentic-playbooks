// Playbooks, runs, profiles and the small read-only lookups (agents, models,
// skills, projects): one function per endpoint, no UI state.

import type {
  OnNoEstimate,
  Project,
  RunDetail,
  RunEstimate,
  RunSummary,
  VersionDiff,
  VersionInfo,
  PlaybookDetail,
  PlaybookSummary,
  RestoredPlaybook,
  RunBudget,
  TrashListEntry,
  TrustEntry,
  TrustKind,
  TrustRevoked,
  WriteResult,
} from '../types'
import type {
  FitReport,
  Graded,
  KnownProblem,
  MachineNotify,
  PlaybookCandidate,
  ProblemDetail,
  ProblemsView,
  QualityRecord,
  RecheckRecord,
  Recall,
  Refused,
  Remembered,
  RunGrades,
  RunNotify,
  ResultSummary,
  ResultView,
  RootInfo,
} from '../api.gen'
import type { RemoveResult, SuggestionRecord } from '../suggestions'
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
  // Agent environment: 'minimal' (the default) or 'full', the opt-in to the
  // operator's whole personal setup. Absent keeps the stored value.
  environment?: 'minimal' | 'full'
  // Which operator environment variables the agent process inherits:
  // 'inherit' (everything) or 'allowlist' (a base set, the agent CLI's own
  // variables and `allow`). Absent keeps the stored value.
  process_env?: { policy?: 'inherit' | 'allowlist'; allow?: string[] }
}

export const fetchProfiles = () =>
  getJson<{ profiles: ProfileSummary[] }>('/api/profiles').then((r) => r.profiles)

export const fetchProfile = (name: string, scope: string, workspace = '') =>
  getJson<ProfileDetail>(`/api/profiles/${encodeURIComponent(name)}${qs({ scope, workspace })}`)

/** What a profile save returns. `skills_unapproved` lists skills the profile
 * kept whose content changed since it was last approved: the save does not
 * vouch for them, so the profile stays untrusted until the user confirms. */
export type ProfileWriteResult = {
  name: string
  trusted?: boolean
  skills_unapproved?: { skill: string; digest: string }[]
}

export const writeProfile = (body: ProfileWriteBody, workspace = '') =>
  requestJson<ProfileWriteResult>(`/api/profiles${qs({ workspace })}`, {
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

export interface ModelRow {
  id: string
  vendor: string
  reasoning?: string | null
}

// One model choice offered for a specific agent (issue #42 finding 9): the
// curated table filtered to that agent's vendor (or the whole table for an
// aggregator), annotated `detected` when the agent's local config/detected
// model list also names it. Detection only annotates or extends this list,
// it never replaces it; for zcode (apb's allowlist) and codex (its static
// list) the list is closed and detection only annotates it - see
// `apb_core::models_table::model_options_for_agent`.
export interface ModelOption {
  id: string
  vendor: string
  detected: boolean
}

/** The agent/model catalog, as the server computes it (`GET /api/models`,
 * `apb_core::agent_catalog`). The server is the only source of truth for
 * these lists: the client never persists them, so a reload always shows what
 * the running `apb` offers. Agents and options come from one snapshot. */
export interface ModelCatalog {
  models: ModelRow[]
  claude_static: string[]
  codex_static: string[]
  agents: AgentInfo[]
  options_by_agent: Record<string, ModelOption[]>
}
export const fetchModelCatalog = () => getJson<ModelCatalog>('/api/models')

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

// Remembered run input of a playbook in a project (`/memory`): the param
// values and instruction a start that passes none uses. `scope` names the
// definition (project or global); the memory always lives in the project.
export type MemoryScope = 'project' | 'global'
export type MemoryView = Recall & { id: string; scope: string; version: string }
export type MemorySaved = Remembered & { id: string; scope: string }
export interface MemoryInput {
  params?: Record<string, string>
  /** An empty string clears the instruction on purpose. */
  instruction?: string
}

export const fetchMemory = (
  id: string,
  workspace = '',
  opts: { scope?: MemoryScope; version?: string } = {},
) => getJson<MemoryView>(`${pb(id)}/memory${qs({ workspace, scope: opts.scope, version: opts.version })}`)

export const saveMemory = (
  id: string,
  input: MemoryInput,
  workspace = '',
  scope?: MemoryScope,
) =>
  requestJson<MemorySaved>(`${pb(id)}/memory${qs({ workspace, scope })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify(input),
  })

export const clearMemory = (
  id: string,
  what: 'all' | 'params' | 'instruction' = 'all',
  workspace = '',
  scope?: MemoryScope,
) =>
  requestJson<{ id: string; scope: string; cleared: string }>(
    `${pb(id)}/memory${qs({ workspace, scope, what })}`,
    { method: 'DELETE' },
  )

// "Will it fit here?": the four-section report of a run in this project.
export const fetchFit = (id: string, workspace = '', scope?: MemoryScope) =>
  getJson<FitReport>(`${pb(id)}/fit${qs({ workspace, scope })}`)

export const fetchRuns = () => getJson<RunSummary[]>('/api/runs')
export const fetchRun = (id: string, workspace = '') =>
  getJson<RunDetail>(`${run(id)}${qs({ workspace })}`)
export const fetchRunReport = (id: string, workspace = '') =>
  getJson<{ report: string }>(`${run(id)}/report${qs({ workspace })}`)

// Run quality: a run's stored grades, a grade of an ended run by rules or
// by the project's decision model (a stored grade re-decides the
// playbook's candidate), a playbook's candidate compared with current, and
// a person's decision on it.
export type GraderChoice = 'rules' | 'model'
export const fetchRunGrades = (id: string, workspace = '') =>
  getJson<RunGrades>(`${run(id)}/grades${qs({ workspace })}`)
export const gradeRun = (id: string, grader: GraderChoice, workspace = '') =>
  requestJson<Graded>(`${run(id)}/grades${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ grader }),
  })
// POST /api/runs/{id}/grades/reopen: a person lets a host grade the run
// once more (a run takes one host grade until then).
export const reopenRunGrading = (id: string, workspace = '') =>
  requestJson<{ run_id: string; reopened: boolean }>(`${run(id)}/grades/reopen${qs({ workspace })}`, {
    method: 'POST',
  })
export const fetchCandidate = (id: string, workspace = '') =>
  getJson<PlaybookCandidate>(`${pb(id)}/candidate${qs({ workspace })}`)
export const promoteCandidate = (id: string, workspace = '') =>
  requestJson<QualityRecord>(`${pb(id)}/candidate/promote${qs({ workspace })}`, { method: 'POST' })
export const keepCurrentCandidate = (id: string, reason: string, workspace = '') =>
  requestJson<QualityRecord>(`${pb(id)}/candidate/keep-current${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(reason.trim() ? { reason: reason.trim() } : {}),
  })

// Known problems: a playbook's problems, one with the passages of its
// source that are new since it was seen, and a person's decision or
// recheck (`verdict`: the person's own recheck, `holds` when it still holds;
// without it the project's decision model rechecks).
export type ProblemAction = 'confirm' | 'dismiss' | 'reopen' | 'close' | 'recheck'
export interface ProblemDecision {
  reason?: string
  quote?: string
  verdict?: 'holds' | 'closed' | 'undetermined'
}
const problems = (id: string) => `${pb(id)}/known-problems`
export const fetchKnownProblems = (id: string, workspace = '') =>
  getJson<ProblemsView>(`${problems(id)}${qs({ workspace })}`)
export const fetchKnownProblem = (id: string, problem: string, workspace = '') =>
  getJson<ProblemDetail>(`${problems(id)}/${encodeURIComponent(problem)}${qs({ workspace })}`)
export const decideKnownProblem = (
  id: string,
  problem: string,
  action: ProblemAction,
  body: ProblemDecision = {},
  workspace = '',
) =>
  requestJson<KnownProblem | RecheckRecord>(
    `${problems(id)}/${encodeURIComponent(problem)}/${action}${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) },
  )

// GET /api/runs/{id}/report.html: the run report as one self-contained page,
// served as a download (the run page's "Save report" link).
export const runReportHtmlUrl = (id: string, workspace = '') =>
  `${run(id)}/report.html${qs({ workspace })}`

// GET /api/notify: the machine's notify target (no secret in it) and
// whether runs of the workspace notify without being asked.
export const fetchNotify = (workspace = '') =>
  getJson<MachineNotify>(`/api/notify${qs({ workspace })}`)

// POST /api/runs/{id}/notify: a person turns a live run's notifications on
// (`true`, through the machine's target or the run's current one) or off.
// Refused with 409 `notify_unavailable` when the target cannot be used, and
// `run_ended` for a run that ended.
export const postRunNotify = (id: string, notify: boolean, workspace = '') =>
  requestJson<{ run_id: string; notify?: RunNotify; warnings?: string[] }>(
    `${run(id)}/notify${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ notify }) },
  )

// POST /api/runs/{id}/review: a gate decision. `nonce` answers a publish
// gate only: the nonce of the plan the person saw (a stale one is a 409
// naming the current plan).
export const postReview = (
  id: string,
  node: string,
  decision: string,
  note = '',
  workspace = '',
  nonce?: string,
) =>
  requestJson<{ posted_seq: number }>(`${run(id)}/review${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify(nonce ? { node, decision, note, nonce } : { node, decision, note }),
  })

// POST /api/runs/{id}/pause (issue #200): the Pause button. `mode` is
// `drain` (default) or `now`; `until` is an RFC 3339 time or a duration.
export interface PauseRequestBody {
  mode?: 'drain' | 'now'
  reason?: string
  until?: string
  keep_lock?: boolean
}
export const pauseRun = (id: string, body: PauseRequestBody = {}, workspace = '') =>
  requestJson<{ run_id: string; outcome: string; mode: string; seq: number; until: number | null }>(
    `${run(id)}/pause${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) },
  )

// POST /api/runs/{id}/resume: the Resume button. An irreversible run with no
// consent recorded is refused with `irreversible_requires_confirmation`; the
// run page answers with its consent dialog and sends the refusal's nonce.
// The budget change of a resume: a raise, a new limit, another policy, or
// letting the node a budget pause holds the run before run once. A plain
// resume of a budget pause is refused (409 `budget_paused`).
export interface ResumeBudget {
  raise_tokens?: number
  raise_usd?: number
  tokens?: number
  usd?: number
  on_no_estimate?: OnNoEstimate
  once?: boolean
}

export const resumeRun = (
  id: string,
  workspace = '',
  opts: { consentNonce?: string; budget?: ResumeBudget } = {},
) =>
  requestJson<{
    run_id: string
    resumed_from: string
    detached: boolean
    resumed_early?: boolean
    pause_until?: number
  }>(`${run(id)}/resume${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({
      ...(opts.consentNonce ? { confirm_irreversible: opts.consentNonce } : {}),
      ...(opts.budget ? { budget: opts.budget } : {}),
    }),
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

// The playbook trash of every project; a restore answers 409 with a readable
// message when a playbook with that id exists again.
export const fetchTrash = () => getJson<TrashListEntry[]>('/api/trash')

export const restoreFromTrash = (name: string, workspace = '') =>
  requestJson<RestoredPlaybook>(
    `/api/trash/${encodeURIComponent(name)}/restore${qs({ workspace })}`,
    { method: 'POST' },
  )

// The approvals in the trust store, and their revocation: `target` is a
// digest (exactly that approval) or an id (every approval under it, narrowed
// by `kind`). A target that matches nothing answers 404.
export const fetchTrust = () => getJson<TrustEntry[]>('/api/trust')

export const revokeTrust = (target: string, kind?: TrustKind) =>
  requestJson<TrustRevoked>('/api/trust/revoke', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ target, kind }),
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

// `consentNonce` is sent only after the person confirmed the playbook's
// irreversible effects in the Run dialog: it is the `consent_nonce` of the
// refusal the dialog showed, so a playbook that changed in between is refused
// again. Without it such a run is refused with
// `irreversible_requires_confirmation` (0.24.0).
// The budget of a run start, over the playbook's `budget:` block. With
// `on_no_estimate` ask and nodes without an estimate the start is refused
// (409 `budget_needs_decision`).
export type { RunBudget } from '../types'

// What a start passes besides its budget: the param values, the
// instruction, whether to remember both for this project, and the "Notify
// me" choice when it differs from what the workspace does anyway.
export interface RunInput {
  params?: Record<string, string>
  instruction?: string
  remember?: boolean
  notify?: boolean
}

export interface RunStarted {
  run_id: string
  warnings?: string[]
  remembered?: string[]
  not_remembered?: Refused[]
  memory_error?: string
}

export const runPlaybook = (
  id: string,
  workspace = '',
  opts: { consentNonce?: string; budget?: RunBudget; input?: RunInput } = {},
) =>
  requestJson<RunStarted>(`${pb(id)}/run${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({
      ...(opts.consentNonce ? { confirm_irreversible: opts.consentNonce } : {}),
      ...(opts.budget ? { budget: opts.budget } : {}),
      ...(opts.input?.params && Object.keys(opts.input.params).length
        ? { params: opts.input.params }
        : {}),
      // An empty instruction is sent on purpose: the person cleared it.
      ...(opts.input?.instruction !== undefined ? { instruction: opts.input.instruction } : {}),
      ...(opts.input?.remember ? { remember: true } : {}),
      ...(opts.input?.notify !== undefined ? { notify: opts.input.notify } : {}),
    }),
  })

// GET /api/playbooks/{id}/estimate: what a run of the version takes and
// spends, from the comparable past runs of its project (`summary` is the
// text `apb run --estimate` prints).
export const fetchEstimate = (id: string, workspace = '', version?: string) =>
  getJson<RunEstimate & { summary: string }>(`${pb(id)}/estimate${qs({ workspace, version })}`)

export const promoteVersion = (id: string, version: string, workspace = '') =>
  requestJson<{ promoted: string }>(
    `${pb(id)}/versions/${encodeURIComponent(version)}/promote${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({}) },
  )

// The whole response, diagnostics included: they are how a corrupt or
// unreadable store becomes visible instead of looking like an empty list.
// An omitted `workspace` asks for the global store on its own, which is the
// only way to reach machine-wide records with zero registered projects.
export interface SuggestionsResponse {
  suggestions: SuggestionRecord[]
  diagnostics?: string[]
}

export const fetchSuggestions = (workspace = '') =>
  getJson<SuggestionsResponse>(`/api/suggestions${qs({ workspace })}`)

export const deleteSuggestion = (pattern: string, workspace = '', scope = 'project') =>
  requestJson<RemoveResult>(
    `/api/suggestions/${encodeURIComponent(pattern)}${qs({ workspace, scope })}`,
    { method: 'DELETE' },
  )

// Connectors (design doc section 9). The server wire shape is snake_case; the

// --- 0.23.0 stats (C3) ---
// Cross-run metrics of one playbook's runs in a project (`apb stats --json`).
export const fetchStats = (playbook: string, workspace = '', since = '', compare = '') =>
  getJson<import('../api.gen').StatsReport>(`/api/stats${qs({ workspace, playbook, since, compare })}`)
// --- end of 0.23.0 stats ---

// History discovery: the step sequences past agent sessions repeat. The
// server answers only a browser on this machine, names roots and projects
// by name or workspace id, and never takes or returns a path.
const discover = '/api/discover'
const suggestion = (id: string, slug: string) =>
  `${discover}/${encodeURIComponent(id)}/${encodeURIComponent(slug)}`
export const fetchDiscoverRoots = () => getJson<RootInfo[]>(`${discover}/roots`)
export const fetchDiscoveries = () => getJson<ResultSummary[]>(discover)
export const fetchDiscovery = (id: string) => getJson<ResultView>(`${discover}/${encodeURIComponent(id)}`)
export interface DiscoverRequestBody {
  workspaces: string[]
  roots: string[]
  since_days?: number
}
export const runDiscovery = (body: DiscoverRequestBody) =>
  requestJson<ResultView>(discover, { method: 'POST', headers: jsonHeaders, body: JSON.stringify(body) })
export const saveDiscovered = (id: string, slug: string, scope: 'project' | 'global', workspace = '') =>
  requestJson<{ id?: string; version?: string; rejected?: string }>(
    `${suggestion(id, slug)}/save${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ scope }) },
  )
export const dismissDiscovered = (id: string, slug: string, kind: 'soft' | 'hard', workspace = '') =>
  requestJson<{ pattern?: string; snoozed_until?: string; rejected?: string }>(
    `${suggestion(id, slug)}/dismiss${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({ kind }) },
  )
