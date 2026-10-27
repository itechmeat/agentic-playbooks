import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  callConnector,
  createPlaybook,
  deletePlaybook,
  fetchDiff,
  fetchModelCatalog,
  clearMemory,
  fetchFit,
  fetchMemory,
  fetchPlaybook,
  postAnswer,
  runPlaybook,
  saveMemory,
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

const H = { 'x-requested-with': 'apb-dashboard' }
const JSON_H = { 'content-type': 'application/json', 'x-requested-with': 'apb-dashboard' }

describe('fetchPlaybook', () => {
  it('GETs /api/playbooks/{id} without version', async () => {
    const detail = { id: 'demo', version: '1.0.1', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', { headers: H })
  })

  it('appends ?version= when version is provided', async () => {
    const detail = { id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo', '', '1.0.0')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0', { headers: H })
  })

  it('encodes version with special characters', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }))
    // `+` is URI-reserved: encodeURIComponent turns it into %2B, so this asserts
    // the query value is actually percent-encoded (a hyphen would pass through).
    await fetchPlaybook('demo', '', '1.0.0+build')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0%2Bbuild', { headers: H })
  })

  it('adds ?workspace= to select a project on the global dashboard', async () => {
    const detail = { id: 'demo', version: '1.0.0', yaml: '', playbook: { id: 'demo', name: 'D', nodes: [], edges: [] }, layout: null, validation: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(detail))
    await fetchPlaybook('demo', 'ws-abc', '1.0.0')
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?workspace=ws-abc&version=1.0.0', { headers: H })
  })
})

describe('postAnswer', () => {
  it('POSTs { node, answer } to /api/runs/{id}/answer', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ posted_seq: 0 }))
    const result = await postAnswer('run-1', { node: 'ask', answer: 'left' })
    expect(result).toEqual({ posted_seq: 0 })
    expect(fetchMock).toHaveBeenCalledWith('/api/runs/run-1/answer', {
      method: 'POST',
      headers: JSON_H,
      body: JSON.stringify({ node: 'ask', answer: 'left' }),
    })
  })

  it('omits node when the caller does not specify one', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ posted_seq: 0 }))
    await postAnswer('run-1', { answer: 'left' })
    const [, init] = fetchMock.mock.calls[0]
    expect((init as RequestInit).body).toBe(JSON.stringify({ answer: 'left' }))
  })

  it('adds ?workspace= to select a project on the global dashboard', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ posted_seq: 0 }))
    await postAnswer('run-1', { node: 'ask', answer: 'left' }, 'ws-abc')
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/runs/run-1/answer?workspace=ws-abc',
      expect.objectContaining({ method: 'POST' }),
    )
  })

  it('surfaces the engine error message on failure', async () => {
    fetchMock.mockResolvedValueOnce(
      new Response('invalid: no pending question to answer (specify a node explicitly)', {
        status: 500,
      }),
    )
    await expect(postAnswer('run-1', { answer: 'left' })).rejects.toThrow(
      /invalid: no pending question to answer/,
    )
  })
})

describe('createPlaybook', () => {
  it('POSTs id and yaml to /api/playbooks', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'demo', version: '1.0.1' }, 201))
    const result = await createPlaybook('demo', 'id: demo\n')
    expect(result).toEqual({ id: 'demo', version: '1.0.1' })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks', {
      method: 'POST',
      headers: JSON_H,
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
      headers: JSON_H,
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
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', { method: 'DELETE', headers: H })
  })
})

describe('saveLayout', () => {
  it('PUTs layout with version query param', async () => {
    fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }))
    const layout = { nodes: [{ id: 'a', x: 10, y: 20 }] }
    await saveLayout('demo', '1.0.0', layout)
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo/layout?version=1.0.0', {
      method: 'PUT',
      headers: JSON_H,
      body: JSON.stringify({ layout }),
    })
  })

  it('throws on non-ok response', async () => {
    fetchMock.mockResolvedValueOnce(new Response('fail', { status: 500 }))
    await expect(saveLayout('demo', '1.0.0', {})).rejects.toThrow()
  })
})

describe('remembered input', () => {
  it('reads, remembers and forgets through /api/playbooks/{id}/memory', async () => {
    const recall = { id: 'p', scope: 'project', version: '1.0.0', instruction: 'x', values: [] }
    fetchMock.mockResolvedValueOnce(jsonResponse(recall))
    await expect(fetchMemory('p')).resolves.toEqual(recall)
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/memory', { headers: H })

    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'p', scope: 'project', stored: ['instruction'], refused: [] }))
    const saved = await saveMemory('p', { instruction: 'hi', params: { channel: 'beta' } })
    expect(saved.stored).toEqual(['instruction'])
    expect(fetchMock).toHaveBeenLastCalledWith('/api/playbooks/p/memory', {
      method: 'PUT',
      headers: JSON_H,
      body: JSON.stringify({ instruction: 'hi', params: { channel: 'beta' } }),
    })

    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'p', scope: 'project', cleared: 'params' }))
    await clearMemory('p', 'params')
    expect(fetchMock).toHaveBeenLastCalledWith('/api/playbooks/p/memory?what=params', {
      method: 'DELETE',
      headers: H,
    })
  })

  it('selects the project and the definition scope', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({}))
    await fetchMemory('p', 'ws-abc', { scope: 'global', version: '2.0.0' })
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/playbooks/p/memory?workspace=ws-abc&scope=global&version=2.0.0',
      { headers: H },
    )
  })
})

describe('fetchFit', () => {
  it('GETs the fit report of a run in the selected project', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: 'p', ready: true }))
    await expect(fetchFit('p', 'ws-abc')).resolves.toMatchObject({ ready: true })
    expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/fit?workspace=ws-abc', { headers: H })
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
      { headers: H },
    )
  })
})

describe('callConnector', () => {
  it('POSTs to /api/connectors/{name}/call with snake_case dry_run and full', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, dry_run: true, method: 'GET', url: 'https://x/items', body: null }))
    await callConnector('mock-tracker', {
      function: 'list_items',
      account: 'acct1',
      args: { q: 'hi' },
      dryRun: true,
      full: false,
    })
    expect(fetchMock).toHaveBeenCalledWith('/api/connectors/mock-tracker/call', {
      method: 'POST',
      headers: JSON_H,
      body: JSON.stringify({
        function: 'list_items',
        account: 'acct1',
        args: { q: 'hi' },
        dry_run: true,
        full: false,
      }),
    })
  })

  it('passes a null account through unchanged (server resolves the default)', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, dry_run: true, method: 'GET', url: 'https://x/items', body: null }))
    await callConnector('mock-tracker', { function: 'ping', account: null, args: {}, dryRun: true, full: false })
    const [, init] = fetchMock.mock.calls[0]
    const sent = JSON.parse((init as RequestInit).body as string)
    expect(sent.account).toBeNull()
  })

  it('passes full: true through when the caller bypasses response_pick', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, status: 200, body: {}, truncated: false, link: null }))
    await callConnector('mock-tracker', { function: 'list_pick', account: 'acct1', args: {}, dryRun: false, full: true })
    const [, init] = fetchMock.mock.calls[0]
    const sent = JSON.parse((init as RequestInit).body as string)
    expect(sent.full).toBe(true)
  })

  it('adds ?workspace= to select a project on the global dashboard', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, status: 200, body: {}, truncated: false, link: null, picked: false }))
    await callConnector('mock-tracker', { function: 'ping', account: 'acct1', args: {}, dryRun: false, full: false }, 'ws-abc')
    expect(fetchMock).toHaveBeenCalledWith(
      '/api/connectors/mock-tracker/call?workspace=ws-abc',
      expect.objectContaining({ method: 'POST' }),
    )
  })
})

describe('401 handling', () => {
  it('flips the auth store and still throws an ApiError', async () => {
    const { auth } = await import('./auth.svelte')
    const { get } = await import('svelte/store')
    auth.set({ required: false, authenticated: true, checked: false })
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'auth' }, 401))
    await expect(fetchPlaybook('demo')).rejects.toThrow(/HTTP 401/)
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})

describe('fetchModelCatalog', () => {
  // The lists change with every build, so no copy may be kept between calls.
  it('asks the server on every call', async () => {
    fetchMock.mockImplementation(async () => jsonResponse({}))
    await fetchModelCatalog()
    await fetchModelCatalog()
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock).toHaveBeenCalledWith('/api/models', { headers: H })
  })
})

describe('runPlaybook', () => {
  it('names the run gate refusal instead of a bare status', async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ policy: 'requires_unmet', id: 'p', missing: ['file:NEEDED.md'] }, 409),
    )
    const err = runPlaybook('p').catch((e: unknown) => e)
    await expect(err).resolves.toMatchObject({ status: 409, code: 'requires_unmet' })
    await expect(err).resolves.toHaveProperty('message', expect.stringMatching(/requires_unmet/))
  })

  // 0.24.0: the irreversible refusal carries its structured sources and
  // consent nonce for the Run dialog, and only the dialog's confirmation
  // sends the consent, as that nonce.
  it('surfaces the irreversible refusal and sends its nonce only when confirmed', async () => {
    const refusal = {
      policy: 'irreversible_requires_confirmation',
      sources: ['node pr', 'sub-playbook node ship'],
      consent_nonce: 'consent-0123',
      detail: 'playbook `p` has irreversible effects (node pr, sub-playbook node ship); a run needs explicit consent: ...',
    }
    fetchMock.mockResolvedValueOnce(jsonResponse(refusal, 409))
    const err = runPlaybook('p').catch((e: unknown) => e)
    await expect(err).resolves.toMatchObject({
      status: 409,
      code: 'irreversible_requires_confirmation',
      body: { sources: refusal.sources, consent_nonce: 'consent-0123' },
    })
    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({})

    fetchMock.mockResolvedValueOnce(jsonResponse({ run_id: 'p-1' }))
    await expect(runPlaybook('p', '', { consentNonce: 'consent-0123' })).resolves.toEqual({
      run_id: 'p-1',
    })
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({
      confirm_irreversible: 'consent-0123',
    })
  })

  // The run budget rides the start, and a start the budget needs a decision
  // for comes back with its structured body (the estimate, the nodes).
  it('sends the budget and surfaces budget_needs_decision with its body', async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ error: 'budget_needs_decision', unestimated: ['a'], options: ['pause', 'continue'] }, 409),
    )
    const err = runPlaybook('p', '', { budget: { tokens: 300000 } }).catch((e: unknown) => e)
    await expect(err).resolves.toMatchObject({
      status: 409,
      code: 'budget_needs_decision',
      body: { unestimated: ['a'] },
    })
    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({
      budget: { tokens: 300000 },
    })
  })

  it('sends the run input and remember only when given', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ run_id: 'p-1', remembered: ['channel'], not_remembered: [] }))
    await runPlaybook('p', '', {
      input: { params: { channel: 'beta' }, instruction: 'go', remember: true },
    })
    expect(JSON.parse(String(fetchMock.mock.calls[0][1]?.body))).toEqual({
      params: { channel: 'beta' },
      instruction: 'go',
      remember: true,
    })
    fetchMock.mockResolvedValueOnce(jsonResponse({ run_id: 'p-2' }))
    await runPlaybook('p', '', { input: { params: {} } })
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({})
  })
})
