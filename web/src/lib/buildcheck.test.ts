import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import { checkServerBuild, staleBuild } from './buildcheck'
import { getJson } from './api/http'

const fetchMock = vi.fn<typeof fetch>()

// The shell this tab loaded names build `aaaa` in <meta name="apb-build">.
function shellBuild(id: string | null) {
  vi.stubGlobal('document', {
    querySelector: (sel: string) =>
      sel === 'meta[name="apb-build"]' && id !== null ? { getAttribute: () => id } : null,
  })
}

function served(build: string, body: unknown = {}) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json', 'x-apb-build': build },
  })
}

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
  staleBuild.set(false)
})

afterEach(() => {
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

describe('stale build detection', () => {
  it('an API answer from the same build keeps the tab current', async () => {
    shellBuild('aaaa')
    fetchMock.mockResolvedValueOnce(served('aaaa'))
    await getJson('/api/runs')
    expect(get(staleBuild)).toBe(false)
  })

  it('an API answer from a rebuilt server marks the tab stale', async () => {
    shellBuild('aaaa')
    fetchMock.mockResolvedValueOnce(served('bbbb'))
    await getJson('/api/runs')
    expect(get(staleBuild)).toBe(true)
  })

  it('the health probe (focus, reconnect) detects a rebuild', async () => {
    shellBuild('aaaa')
    fetchMock.mockResolvedValueOnce(served('bbbb', { status: 'ok', build_id: 'bbbb' }))
    await checkServerBuild()
    expect(get(staleBuild)).toBe(true)
  })

  it('a shell without a build id (vite dev server) never reads as stale', async () => {
    shellBuild(null)
    fetchMock.mockResolvedValueOnce(served('bbbb'))
    await getJson('/api/runs')
    expect(get(staleBuild)).toBe(false)
  })
})
