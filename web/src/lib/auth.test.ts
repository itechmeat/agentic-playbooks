import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import {
  apiHeaders,
  auth,
  login,
  logout,
  markUnauthenticated,
  refreshAuthStatus,
  XRW_HEADER,
  XRW_VALUE,
} from './auth'

const fetchMock = vi.fn<typeof fetch>()

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
  auth.set({ required: false, authenticated: true, checked: false })
})

afterEach(() => {
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

describe('apiHeaders', () => {
  it('always carries the marker header', () => {
    expect(apiHeaders()).toEqual({ [XRW_HEADER]: XRW_VALUE })
  })

  it('merges extra headers without losing the marker', () => {
    expect(apiHeaders({ 'content-type': 'application/json' })).toEqual({
      'content-type': 'application/json',
      [XRW_HEADER]: XRW_VALUE,
    })
  })
})

describe('refreshAuthStatus', () => {
  it('stores what the server reports', async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ auth_required: true, authenticated: false }),
    )
    const state = await refreshAuthStatus()
    expect(state).toEqual({ required: true, authenticated: false, checked: true })
    expect(get(auth)).toEqual(state)
    expect(fetchMock).toHaveBeenCalledWith('/api/auth/status', {
      headers: { [XRW_HEADER]: XRW_VALUE },
    })
  })

  it('treats an unreachable or unknown endpoint as auth off', async () => {
    fetchMock.mockResolvedValueOnce(new Response('nope', { status: 404 }))
    const state = await refreshAuthStatus()
    expect(state).toEqual({ required: false, authenticated: true, checked: true })
  })
})

describe('login', () => {
  it('posts the key and refreshes the status on success', async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse({ authenticated: true }))
      .mockResolvedValueOnce(jsonResponse({ auth_required: true, authenticated: true }))
    const result = await login('apb_secret')
    expect(result.ok).toBe(true)
    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/auth/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json', [XRW_HEADER]: XRW_VALUE },
      body: JSON.stringify({ key: 'apb_secret' }),
    })
    expect(get(auth)).toEqual({ required: true, authenticated: true, checked: true })
  })

  it('reports a rejected key without touching the store', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'auth' }, 401))
    const result = await login('apb_wrong')
    expect(result.ok).toBe(false)
    expect(result.message).toMatch(/not accepted/)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('reports rate limiting distinctly', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'rate_limited' }, 429))
    const result = await login('apb_wrong')
    expect(result.ok).toBe(false)
    expect(result.message).toMatch(/Too many attempts/)
  })
})

describe('logout', () => {
  it('posts logout and re-reads the status', async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse({ authenticated: false }))
      .mockResolvedValueOnce(jsonResponse({ auth_required: true, authenticated: false }))
    await logout()
    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/auth/logout', {
      method: 'POST',
      headers: { [XRW_HEADER]: XRW_VALUE },
    })
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})

describe('markUnauthenticated', () => {
  it('flips the store into the login state', () => {
    markUnauthenticated()
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})
