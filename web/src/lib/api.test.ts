import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  createPlaybook,
  deletePlaybook,
  fetchDiff,
  fetchInputDraft,
  fetchPlaybook,
  saveInputDraft,
  saveLayout,
  updatePlaybook,
} from './api'

const fetchMock = vi.fn<typeof fetch>()

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
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

describe('fetchPlaybook', () => {
  it('GETs /api/playbooks/{id} without version', async () => {
    const detail = { id: 'demo', version: '1.0.1', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo')
  })

  it('appends ?version= when version is provided', async () => {
    const detail = { id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo', '', '1.0.0')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0')
  })

  it('encodes version with special characters', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }))
    // `+` is URI-reserved: encodeURIComponent turns it into %2B, so this asserts
    // the query value is actually percent-encoded (a hyphen would pass through).
    await fetchPlaybook('demo', '', '1.0.0+build')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0%2Bbuild')
  })

  it('adds ?workspace= to select a project on the global dashboard', async () => {
    const detail = { id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo', 'ws-abc', '1.0.0')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?workspace=ws-abc&version=1.0.0')
  })
})

describe('createPlaybook', () => {
  it('POSTs id and yaml to /api/playbooks', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'demo', version: '1.0.1' }, 201))
    const result = await createPlaybook('demo', 'id: demo\n')
    expect(result).toEqual({ id: 'demo', version: '1.0.1' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id: 'demo', yaml: 'id: demo\n' }),
    })
  })

  it('throws on non-ok response', async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ error: 'validation', codes: ['missing_start'] }, 400),
    )
    await expect(createPlaybook('demo', 'bad')).rejects.toThrow(/validation/)
  })
})

describe('updatePlaybook', () => {
  it('PUTs yaml to /api/playbooks/{id}', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'demo', version: '1.0.2' }))
    const result = await updatePlaybook('demo', 'id: demo\nnodes: []\n')
    expect(result).toEqual({ id: 'demo', version: '1.0.2' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ yaml: 'id: demo\nnodes: []\n' }),
    })
  })

  it('encodes special characters in id', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'a b', version: '1.0.0' }))
    await updatePlaybook('a b', 'yaml')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/a%20b', expect.any(Object))
  })
})

describe('deletePlaybook', () => {
  it('DELETEs /api/playbooks/{id} and returns trashed path', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ trashed: '.apb/trash/demo' }))
    const result = await deletePlaybook('demo')
    expect(result).toEqual({ trashed: '.apb/trash/demo' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', { method: 'DELETE' })
  })
})

describe('saveLayout', () => {
  it('PUTs layout with version query param', async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }))
    const layout = { nodes: [{ id: 'a', x: 10, y: 20 }] }
    await saveLayout('demo', '1.0.0', layout)
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo/layout?version=1.0.0', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ layout }),
    })
  })

  it('throws on non-ok response', async () => {
    fetchMock.mockResolvedValueOnce(new Response('fail', { status: 500 }))
    await expect(saveLayout('demo', '1.0.0', {})).rejects.toThrow()
  })
})

describe('input draft', () => {
  it('GETs and PUTs /api/playbooks/{id}/input-draft', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ instruction: 'x' }))
    const got = await fetchInputDraft('p')
    expect(got).toEqual({ instruction: 'x' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft')

    fetchMock.mockResolvedValueOnce(jsonResponse({ instruction: 'hi' }))
    const saved = await saveInputDraft('p', 'hi')
    expect(saved).toEqual({ instruction: 'hi' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ instruction: 'hi' }),
    })
  })

  it('adds ?workspace= to select a project on the global dashboard', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ instruction: null }))
    await fetchInputDraft('p', 'ws-abc')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft?workspace=ws-abc')

    fetchMock.mockResolvedValueOnce(jsonResponse({ instruction: null }))
    await saveInputDraft('p', 'hi', 'ws-abc')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft?workspace=ws-abc', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ instruction: 'hi' }),
    })
  })
})

describe('fetchDiff', () => {
  it('GETs diff with from and to query params', async () => {
    const diff = {
      nodes_added: ['a'],
      nodes_removed: [],
      nodes_changed: [],
      edges_added: [],
      edges_removed: [],
      yaml_diff: '+added\n',
    }
    fetchMock.mockResolvedValueOnce(jsonResponse(diff))
    const result = await fetchDiff('demo', '1.0.0', '1.0.1')
    expect(result).toEqual(diff)
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/playbooks/demo/diff?from=1.0.0&to=1.0.1',
    )
  })
})
