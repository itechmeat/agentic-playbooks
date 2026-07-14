import type {
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

async function requestJson<T>(url: string, init: RequestInit): Promise<T> {
  const res = await fetch(url, init)
  if (!res.ok) {
    const msg = await errorMessage(res)
    throw new Error(msg)
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
    if (body.error) return `${url}: ${body.error}`
  } catch {
    // body is not JSON
  }
  return `${url}: HTTP ${res.status}`
}

const jsonHeaders = { 'content-type': 'application/json' }

export const fetchPlaybooks = () => getJson<PlaybookSummary[]>('/api/playbooks')
export const fetchPlaybook = (id: string, version?: string) =>
  getJson<PlaybookDetail>(
    `/api/playbooks/${encodeURIComponent(id)}${version ? `?version=${encodeURIComponent(version)}` : ''}`,
  )

export const fetchRuns = () => getJson<RunSummary[]>('/api/runs')
export const fetchRun = (id: string) =>
  getJson<RunDetail>(`/api/runs/${encodeURIComponent(id)}`)
export const fetchRunReport = (id: string) =>
  getJson<{ report: string }>(`/api/runs/${encodeURIComponent(id)}/report`)

export const postReview = (id: string, node: string, decision: string, note = '') =>
  requestJson<{ posted_seq: number }>(`/api/runs/${encodeURIComponent(id)}/review`, {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ node, decision, note }),
  })

export const createPlaybook = (id: string, yaml: string) =>
  requestJson<WriteResult>('/api/playbooks', {
    method: 'POST',
    headers: jsonHeaders,
    body: JSON.stringify({ id, yaml }),
  })

export const updatePlaybook = (id: string, yaml: string) =>
  requestJson<WriteResult>(`/api/playbooks/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ yaml }),
  })

export const deletePlaybook = (id: string) =>
  requestJson<{ trashed: string }>(`/api/playbooks/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })

export const saveLayout = (id: string, version: string, layout: unknown) =>
  requestJson<void>(
    `/api/playbooks/${encodeURIComponent(id)}/layout?version=${encodeURIComponent(version)}`,
    {
      method: 'PUT',
      headers: jsonHeaders,
      body: JSON.stringify({ layout }),
    },
  )

export const fetchDiff = (id: string, from: string, to: string) =>
  getJson<VersionDiff>(
    `/api/playbooks/${encodeURIComponent(id)}/diff?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`,
  )

export const fetchVersions = (id: string) =>
  getJson<VersionInfo[]>(`/api/playbooks/${encodeURIComponent(id)}/versions`)

export const promoteVersion = (id: string, version: string) =>
  requestJson<{ promoted: string }>(
    `/api/playbooks/${encodeURIComponent(id)}/versions/${encodeURIComponent(version)}/promote`,
    { method: 'POST', headers: jsonHeaders, body: JSON.stringify({}) },
  )
