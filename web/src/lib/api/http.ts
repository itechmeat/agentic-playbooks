// The fetch layer every api module shares: JSON request/response handling,
// a single place where a failed response becomes a readable Error, and the
// two URL builders that keep ids escaped.
//
// Every request carries the CSRF marker header, and every 401 flips the shared
// auth store so the app can show the login screen instead of a toast nobody
// can act on. Both live in `../auth.svelte`, which imports nothing from here.

import { apiHeaders, markUnauthenticated } from '../auth.svelte'

export async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, { headers: apiHeaders() })
  if (!res.ok) {
    if (res.status === 401) markUnauthenticated()
    throw new ApiError(`${url}: HTTP ${res.status}`, res.status)
  }
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

export async function requestJson<T>(url: string, init: RequestInit): Promise<T> {
  const headers = apiHeaders(init.headers as Record<string, string> | undefined)
  const res = await fetch(url, { ...init, headers })
  if (!res.ok) {
    if (res.status === 401) markUnauthenticated()
    const err = await errorMessage(res)
    throw new ApiError(err.message, res.status, err.code, err.detail)
  }
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}

export async function errorMessage(
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

export const jsonHeaders = { 'content-type': 'application/json' }

// Builds a query string from defined, non-empty params. `workspace` selects the
// project on the global dashboard; it is omitted when empty (pinned-root server).
export function qs(params: Record<string, string | undefined>): string {
  const parts = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== '')
    .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v as string)}`)
  return parts.length ? `?${parts.join('&')}` : ''
}

export const pb = (id: string) => `/api/playbooks/${encodeURIComponent(id)}`
export const run = (id: string) => `/api/runs/${encodeURIComponent(id)}`
