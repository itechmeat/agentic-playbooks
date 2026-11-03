// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import RunResultsPanel from './RunResultsPanel.svelte'
import type { ResultsView } from './results'

let app: ReturnType<typeof mount> | null = null
const fetchMock = vi.fn<typeof fetch>()
beforeEach(() => vi.stubGlobal('fetch', fetchMock))
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

const snap = 'c'.repeat(64)
const view: ResultsView = {
  rows: [
    {
      execution: { node: 'report', ordinal: 2 },
      settlement_seq: 30,
      produced_by_attempt: 21,
      attempt_number: 2,
      status: 'failed',
      snapshot_id: snap,
      entries: [
        { entry: 'output', part: 'output', size: 120, digest: 'sha256:' + '1'.repeat(64), redacted: false, format: 'text' },
        {
          entry: 'file:workspace:docs/a%20b.md',
          part: 'file',
          scope: 'workspace',
          path: 'docs/a b.md',
          size: 6,
          digest: 'sha256:' + '2'.repeat(64),
          redacted: true,
          format: 'text',
        },
        {
          entry: 'file:run:%3Credacted%20path%201%3E',
          part: 'file',
          scope: 'run',
          path: '<redacted path 1>',
          size: 4,
          digest: 'sha256:' + '3'.repeat(64),
          redacted: false,
          format: 'text',
        },
      ],
      omitted: [{ part: 'file', scope: 'workspace', path: '.env', reason: 'secret_shaped' }],
      incomplete: 'enumeration_limit',
      provenance: { cache_hit: false, head: '0123456789abcdef0123456789abcdef01234567' },
      verified: 'ok',
    },
    {
      execution: { node: 'items', ordinal: 1 },
      item: { visit: { node: 'items', started_seq: 5 }, id: 'pr-7' },
      settlement_seq: 12,
      status: 'passed',
      snapshot_id: 'd'.repeat(64),
      // A corrupt object: nothing verifies a size or digest, so none is served.
      entries: [{ entry: 'output', part: 'output', redacted: false, format: 'text' }],
      omitted: [],
      provenance: { cache_hit: true },
      verified: 'corrupt',
    },
  ],
  legacy: [{ execution: { node: 'old', ordinal: 1 }, settlement_seq: 3, scope: 'workspace', path: 'out.txt', digest: 'sha256:' + '5'.repeat(64) }],
  not_stored: [],
}

function texts(sel: string): string[] {
  return [...document.querySelectorAll(sel)].map((e) => (e.textContent ?? '').replace(/\s+/g, ' ').trim())
}

describe('RunResultsPanel', () => {
  it('shows each node, execution and item with its entries, badges, omissions and the legacy digests', () => {
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view } })
    flushSync()
    expect(texts('[data-testid="run-results-index"] button')).toEqual(['items Results (1)', 'report Results (1)'])
    expect(texts('[data-testid="run-results-group"] > h4')).toEqual(['items', 'report'])
    const rows = texts('[data-testid="run-results-row"]')
    expect(rows[0]).toContain('#1 · item pr-7')
    expect(rows[1]).toContain('#2')
    // The attempt's number, as the commit rows name it, never its id (a seq).
    expect(rows[1]).toContain('attempt 2')
    expect(rows[1]).not.toContain('attempt 21')
    expect(rows[1]).toContain('taken at 0123456789ab')
    expect(texts('[data-testid="run-results-badge"]')).toEqual([
      'corrupt',
      'cache hit',
      'redacted',
      'incomplete: enumeration limit',
      'omitted 1',
    ])
    expect(texts('[data-testid="run-results-omission"]')).toEqual(['file:workspace:.env: secret shaped'])
    expect(texts('[data-testid="run-results-entry-name"]')).toEqual([
      'output',
      'output',
      'file:workspace:docs/a b.md',
      'file:run:<redacted path 1> path redacted',
    ])
    // A corrupt snapshot offers no content.
    const first = document.querySelectorAll('[data-testid="run-results-row"]')[0]
    expect(first.querySelector('[data-testid="run-results-view"]')).toBeNull()
    expect(texts('[data-testid="run-results-entry-stored"]')).toEqual([
      'unverified',
      '120 B sha256:111111111111',
      '6 B sha256:222222222222',
      '4 B sha256:333333333333',
    ])
    // Comparison only for a workspace file; download links the raw route.
    expect(document.querySelectorAll('[data-testid="run-results-compare"]').length).toBe(1)
    const download = document.querySelector<HTMLAnchorElement>('[data-testid="run-results-download"]')
    expect(download?.getAttribute('href')).toBe(`/api/runs/r1/results/${snap}/raw?entry=output`)
    expect(texts('[data-testid="run-results-legacy"] li')).toEqual(['old #1 · file:workspace:out.txt · sha256:555555555555'])
  })

  it('shows stored text as text, with the cap note, and never renders it as markup', async () => {
    const text = '<script>window.__pwned = 1</script><b>bold</b>'
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          run_id: 'r1',
          ref: `r:${snap}/output`,
          content: {
            text,
            digest: 'sha256:' + '1'.repeat(64),
            size: 70000,
            range: [0, 65536],
            redacted_at_capture: false,
            redacted_on_read: true,
          },
        }),
        { status: 200 },
      ),
    )
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view } })
    flushSync()
    document.querySelector<HTMLButtonElement>('[data-testid="run-results-view"]')?.click()
    await vi.waitFor(() => expect(document.querySelector('[data-testid="run-results-text"]')).not.toBeNull())
    expect(fetchMock).toHaveBeenCalledWith(`/api/runs/r1/results/${snap}?entry=output&offset=0&limit=65536`, expect.anything())
    const pre = document.querySelector('[data-testid="run-results-text"]')!
    expect(pre.tagName).toBe('PRE')
    expect(pre.textContent).toBe(text)
    expect(pre.querySelector('script, b')).toBeNull()
    expect(document.body.textContent).toContain('Showing bytes 0 to 65536 of 70000. Download for the whole entry.')
    expect(document.body.textContent).toContain('redacted on read')
  })

  it('shows a failed read and a failed comparison by the server sentence, without the request address', async () => {
    const refusal = (error: string) =>
      new Response(JSON.stringify({ error }), { status: 409, headers: { 'content-type': 'application/json' } })
    fetchMock.mockResolvedValueOnce(refusal('corrupt: the stored snapshot or its object is missing'))
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view } })
    flushSync()
    document.querySelector<HTMLButtonElement>('[data-testid="run-results-view"]')?.click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('corrupt: the stored snapshot or its object is missing'))
    expect(document.body.textContent).not.toContain('/api/runs/')
    fetchMock.mockResolvedValueOnce(refusal('corrupt: the object does not hash to its name'))
    document.querySelector<HTMLButtonElement>('[data-testid="run-results-compare"]')?.click()
    await vi.waitFor(() =>
      expect(texts('[data-testid="run-results-live"]')).toEqual(['comparison failed: corrupt: the object does not hash to its name']),
    )
    await tick()
  })

  it('answers a live comparison with a word only', async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ live: 'differs' }), { status: 200 }))
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view } })
    flushSync()
    document.querySelector<HTMLButtonElement>('[data-testid="run-results-compare"]')?.click()
    await vi.waitFor(() => expect(texts('[data-testid="run-results-live"]')).toEqual(['differs from the live file']))
    await tick()
  })

  it('says which settlements kept no snapshot and why, even with nothing else to show', () => {
    const failed: ResultsView = {
      rows: [],
      legacy: [],
      not_stored: [
        { node: 'work', execution: 2, reason: 'snapshot not stored: the store is full' },
        { node: 'each', execution: 1, item: 'a', reason: 'snapshot not stored: a lock did not come in time' },
      ],
    }
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view: failed } })
    flushSync()
    expect(texts('[data-testid="run-results-not-stored"] li')).toEqual([
      'work #2: snapshot not stored: the store is full',
      'each #1 item a: snapshot not stored: a lock did not come in time',
    ])
  })

  it('renders nothing for a run without snapshots or recorded digests', () => {
    app = mount(RunResultsPanel, { target: document.body, props: { runId: 'r1', view: { rows: [], legacy: [], not_stored: [] } } })
    flushSync()
    expect(document.querySelector('[data-testid="run-results"]')).toBeNull()
  })
})
