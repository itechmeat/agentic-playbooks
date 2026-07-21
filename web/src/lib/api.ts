import type {
  Project,
  RunDetail,
  RunSummary,
  VersionDiff,
  VersionInfo,
  PlaybookDetail,
  PlaybookSummary,
  WriteResult,
} from './types'
import type {
  ConnectorAccount,
  ConnectorCard,
  ConnectorDetail,
  ConnectorFunction,
  ConnectorMeta,
  ConnectorTrust,
  JsonSchema,
} from './connectors'
import type { AvailableConnector, InstallResult, UninstallResult } from './connectorinstall'
import type { ConnectorFunctionStat, ConnectorStats } from './connectorstats'
import type { PlayCallResult } from './connectorplay'

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) throw new ApiError(`${url}: HTTP ${res.status}`, res.status)
  return res.json() as Promise<T>
}

/// An error carrying the HTTP status, so callers can branch on it structurally
/// (e.g. a 409 conflict) instead of matching substrings in the message. `code`
/// is the machine-readable `error` field of the JSON body when the server sent
/// one, so a caller can map a documented code to its own copy.
export class ApiError extends Error {
  status: number
  code?: string
  detail?: string
  constructor(message: string, status: number, code?: string, detail?: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.detail = detail
  }
}

async function requestJson<T>(url: string, init: RequestInit): Promise<T> {
  const res = await fetch(url, init)
  if (!res.ok) {
    const err = await errorMessage(res)
    throw new ApiError(err.message, res.status, err.code, err.detail)
  }
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}

async function errorMessage(
  res: Response,
): Promise<{ message: string; code?: string; detail?: string }> {
  const url = res.url || ''
  const text = await res.text().catch(() => '')
  try {
    const body = JSON.parse(text) as {
      error?: string
      codes?: string[]
      message?: string
      detail?: string
    }
    const meta = { code: body.error, detail: body.detail }
    if (body.error === 'validation' && body.codes?.length) {
      return { message: `${url}: validation: ${body.codes.join(', ')}`, ...meta }
    }
    if (body.error === 'schema' && body.message) {
      return { message: `${url}: schema: ${body.message}`, ...meta }
    }
    if (body.error === 'frozen') return { message: `${url}: playbook is frozen`, ...meta }
    if (body.error) return { message: `${url}: ${body.error}`, ...meta }
  } catch {
    // body is not JSON: a plain-text body (e.g. the answer endpoint's
    // answer_by relay diagnostic) is still worth surfacing verbatim rather
    // than collapsing to a bare status code.
    if (text.trim()) return { message: `${url}: ${text.trim()}` }
  }
  return { message: `${url}: HTTP ${res.status}` }
}

const jsonHeaders = { 'content-type': 'application/json' }

// Builds a query string from defined, non-empty params. `workspace` selects the
// project on the global dashboard; it is omitted when empty (pinned-root server).
function qs(params: Record<string, string | undefined>): string {
  const parts = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== '')
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v as string)}`)
  return parts.length ? `?${parts.join('&')}` : ''
}

const pb = (id: string) => `/api/playbooks/${encodeURIComponent(id)}`
const run = (id: string) => `/api/runs/${encodeURIComponent(id)}`

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
export const fetchModels = () =>
  getJson<{ models: ModelRow[]; claude_static: string[] }>('/api/models')

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
// dashboard types in `./connectors` are camelCase, so the mapping happens
// here, at the fetch boundary, the same way the rest of this file owns the
// wire<->UI shape.
const conn = (name: string) => `/api/connectors/${encodeURIComponent(name)}`

interface ConnectorCardDto {
  name: string
  version: string
  display_name: string
  summary: string
  tags: string[]
  trust: ConnectorTrust
  accounts_total: number
  accounts_ready: number
}

const toConnectorCard = (d: ConnectorCardDto): ConnectorCard => ({
  name: d.name,
  version: d.version,
  displayName: d.display_name,
  summary: d.summary,
  tags: d.tags,
  trust: d.trust,
  accountsTotal: d.accounts_total,
  accountsReady: d.accounts_ready,
})

export const fetchConnectors = (workspace = '') =>
  getJson<ConnectorCardDto[]>(`/api/connectors${qs({ workspace })}`).then((list) =>
    list.map(toConnectorCard),
  )

interface ConnectorAccountDto {
  name: string
  default: boolean
  fields: Record<string, string>
  missing_env: string[]
  trust: ConnectorTrust
}

const toConnectorAccount = (d: ConnectorAccountDto): ConnectorAccount => ({
  name: d.name,
  default: d.default,
  fields: d.fields,
  missingEnv: d.missing_env,
  trust: d.trust,
})

interface ConnectorFunctionDto {
  name: string
  description: string
  read_only: boolean
  deprecated: boolean
  args_schema?: JsonSchema | null
}

const toConnectorFunction = (d: ConnectorFunctionDto): ConnectorFunction => ({
  name: d.name,
  description: d.description,
  readOnly: d.read_only,
  deprecated: d.deprecated,
  argsSchema: d.args_schema ?? null,
})

interface ConnectorDetailDto {
  name: string
  version: string
  installed: boolean
  trust: ConnectorTrust
  meta: ConnectorMeta
  body_md: string
  functions: ConnectorFunctionDto[]
  accounts: ConnectorAccountDto[]
}

export const fetchConnector = (name: string, workspace = '') =>
  getJson<ConnectorDetailDto>(`${conn(name)}${qs({ workspace })}`).then(
    (d): ConnectorDetail => ({
      name: d.name,
      version: d.version,
      installed: d.installed,
      trust: d.trust,
      meta: d.meta,
      bodyMd: d.body_md,
      functions: d.functions.map(toConnectorFunction),
      accounts: d.accounts.map(toConnectorAccount),
    }),
  )

export interface HealthcheckError {
  code: string
  message: string
  http_status?: number
  retry_after_sec?: number
}
export interface HealthcheckResult {
  ok: boolean
  error?: HealthcheckError
  [key: string]: unknown
}

// The executor's structured outcome, returned verbatim (design doc section
// 9/8). The server answers HTTP 200 even for failures, so a trust-gated
// refusal arrives as a normal `ok:false` body with `error.code === "permission"`,
// never as an HTTP error. requestJson's non-ok branch only fires on
// transport-level or server-level HTTP errors, not on healthcheck outcomes.
export const runConnectorHealthcheck = (name: string, account: string, workspace = '') =>
  requestJson<HealthcheckResult>(
    `${conn(name)}/healthcheck/${encodeURIComponent(account)}${qs({ workspace })}`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({}) },
  )

interface AvailableConnectorDto {
  name: string
  version: string
  display_name: string
  summary: string
  tags: string[]
}

// GET /api/connectors/available: the embedded official connectors that are NOT
// installed. Always 200; an empty array means everything is already installed.
export const fetchAvailableConnectors = () =>
  getJson<AvailableConnectorDto[]>('/api/connectors/available').then((list) =>
    list.map(
      (d): AvailableConnector => ({
        name: d.name,
        version: d.version,
        displayName: d.display_name,
        summary: d.summary,
        tags: d.tags,
      }),
    ),
  )

interface InstallResultDto {
  ok: boolean
  name: string
  version: string
  digest: string
  no_op: boolean
  trust_recorded: boolean
  trust_warning: string | null
}

// POST /api/connectors/{name}/install: `force` replaces a different installed
// version, which the server otherwise refuses with 409 needs_force. Only ever
// sent as a deliberate user action, never as an automatic retry.
export const installConnector = (name: string, force = false) =>
  requestJson<InstallResultDto>(`${conn(name)}/install${qs({ force: force ? 'true' : '' })}`, {
    method: 'POST',
  }).then(
    (d): InstallResult => ({
      ok: d.ok,
      name: d.name,
      version: d.version,
      digest: d.digest,
      noOp: d.no_op,
      trustRecorded: d.trust_recorded,
      trustWarning: d.trust_warning,
    }),
  )

// POST /api/connectors/{name}/uninstall: removes the connector tree only. The
// account configuration lives in a separate store and is left untouched.
export const uninstallConnector = (name: string) =>
  requestJson<{ ok: boolean; name: string; no_op: boolean }>(`${conn(name)}/uninstall`, {
    method: 'POST',
  }).then((d): UninstallResult => ({ ok: d.ok, name: d.name, noOp: d.no_op }))

export const approveConnector = (name: string, account: string | null = null, workspace = '') =>
  requestJson<{ ok: boolean }>(`/api/connectors/approve${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ name, account }),
  })

interface ConnectorFunctionStatDto {
  function: string
  account: string
  calls: number
  errors: number
  avg_duration_ms: number
}

interface ConnectorStatsDto {
  connector: string
  runs_scanned: number
  calls: number
  by_function: ConnectorFunctionStatDto[]
  by_outcome: Record<string, number>
}

const toConnectorFunctionStat = (d: ConnectorFunctionStatDto): ConnectorFunctionStat => ({
  function: d.function,
  account: d.account,
  calls: d.calls,
  errors: d.errors,
  avgDurationMs: d.avg_duration_ms,
})

// GET /api/connectors/{name}/stats: usage stats aggregated server-side from
// recent run event logs (design doc section 9). Read-only; the server bounds
// the run scan itself, `runsScanned` reports how many it actually read.
export const fetchConnectorStats = (name: string, workspace = '') =>
  getJson<ConnectorStatsDto>(`${conn(name)}/stats${qs({ workspace })}`).then(
    (d): ConnectorStats => ({
      connector: d.connector,
      runsScanned: d.runs_scanned,
      calls: d.calls,
      byFunction: d.by_function.map(toConnectorFunctionStat),
      byOutcome: d.by_outcome,
    }),
  )

export interface PlayCallRequest {
  function: string
  account: string | null
  args: Record<string, unknown>
  dryRun: boolean
  // Bypasses the function's response_pick projection (spec 4.5 / 2026-07-19
  // section 7 post-review fix), mirroring the CLI's --full debugging
  // escape. false (the playground default) applies the projection like a
  // normal agent call, so a projected function's `picked` flag reads true.
  full: boolean
}

interface PlayCallRequestDto {
  function: string
  account: string | null
  args: Record<string, unknown>
  dry_run: boolean
  full: boolean
}

// POST /api/connectors/{name}/call: the dashboard playground's manual call
// (design doc 2026-07-19-official-connectors-design section 7). Wraps the
// same live execution path the healthcheck probe uses, extended with an
// arbitrary function, args, a dry-run flag, and a full flag. Like the
// healthcheck probe, the server answers HTTP 200 even for a refused or
// failed call - the outcome is carried in the body's `ok`/`error`, never as
// an HTTP error.
export const callConnector = (name: string, req: PlayCallRequest, workspace = '') =>
  requestJson<PlayCallResult>(`${conn(name)}/call${qs({ workspace })}`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({
      function: req.function,
      account: req.account,
      args: req.args,
      dry_run: req.dryRun,
      full: req.full,
    } satisfies PlayCallRequestDto),
  })
