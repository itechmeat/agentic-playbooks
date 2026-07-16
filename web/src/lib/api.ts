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

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`)
  return res.json() as Promise<T>
}

/// An error carrying the HTTP status, so callers can branch on it structurally
/// (e.g. a 409 conflict) instead of matching substrings in the message.
export class ApiError extends Error {
  status: number
  constructor(message: string, status: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

async function requestJson<T>(url: string, init: RequestInit): Promise<T> {
  const res = await fetch(url, init)
  if (!res.ok) {
    const msg = await errorMessage(res)
    throw new ApiError(msg, res.status)
  }
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}

async function errorMessage(res: Response): Promise<string> {
  const url = res.url || ''
  try {
    const body = (await res.json()) as { error?: string; codes?: string[]; message?: string }
    if (body.error === 'validation' && body.codes?.length) {
      return `${url}: validation: ${body.codes.join(', ')}`
    }
    if (body.error === 'schema' && body.message) {
      return `${url}: schema: ${body.message}`
    }
    if (body.error === 'frozen') return `${url}: playbook is frozen`
    if (body.error) return `${url}: ${body.error}`
  } catch {
    // body is not JSON
  }
  return `${url}: HTTP ${res.status}`
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
