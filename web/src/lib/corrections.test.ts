// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushSync, mount, tick, unmount } from 'svelte'
import CorrectionsCard from './CorrectionsCard.svelte'

// The corrections card reads GET /api/playbooks/{id}/corrections and posts
// a person's decisions to .../{entry}/{action}: the test stubs `fetch` and
// checks the requests it sends and what it shows of each answer.
const fetchMock = vi.fn<typeof fetch>()
let app: ReturnType<typeof mount> | null = null
beforeEach(() => vi.stubGlobal('fetch', fetchMock))
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const plain = (text: string, status: number) => new Response(text, { status })

// The shapes of GET /api/playbooks/{id}/corrections, kept as local
// structural copies of what the card declares (the generated contract does
// not carry the corrections types yet).
interface Decided {
  by: string
  at_ms: number
  reason?: string
}
interface FixRecord {
  version: string
  base_version: string
  base_runs: number
  base_hits: number
  marked: Decided
}
interface Correction {
  id: string
  kind: string
  node: string | null
  gate: string | null
  text: string | null
  runs: number
  via_supervisor: boolean
  status: 'open' | 'proposed' | 'fixed' | 'reopened' | 'dismissed'
  fix?: FixRecord | null
  last_version?: string
}
interface Draft {
  node: string
  line: string
}
interface FixRate {
  base_runs: number
  base_hits: number
  fix_runs: number
  fix_hits: number
}
interface CorrectionView {
  correction: Correction
  draft: Draft | null
  rate: FixRate | null
}
interface CorrectionsView {
  playbook: string
  scope: string
  unsigned_entries: number
  signed: boolean
  entries: CorrectionView[]
}

const decided = { by: 'dashboard', at_ms: 1, reason: '' }
const FIX = { version: '1.2.0', base_version: '1.1.0', base_runs: 2, base_hits: 2, marked: decided }

function entry(opts: { correction?: Partial<Correction>; draft?: Draft | null; rate?: FixRate | null } = {}): CorrectionView {
  return {
    correction: {
      id: 'c-x',
      kind: 'review_note',
      node: 'implement',
      gate: 'review',
      text: 'add tests for the parser',
      runs: 2,
      via_supervisor: false,
      status: 'proposed',
      ...opts.correction,
    },
    draft: opts.draft ?? null,
    rate: opts.rate ?? null,
  }
}

function list(over: Partial<CorrectionsView> = {}): CorrectionsView {
  return {
    playbook: 'fix-build',
    scope: 'project',
    signed: true,
    unsigned_entries: 0,
    entries: [],
    ...over,
  }
}

const DRAFT_LINE = 'A person corrected this step in earlier runs: add tests for the parser'

async function settle() {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve()
    await tick()
  }
  flushSync()
}

async function render(v: CorrectionsView, opts: { versions?: string[] } = {}) {
  fetchMock.mockResolvedValueOnce(json(v))
  app = mount(CorrectionsCard, {
    target: document.body,
    props: { id: 'fix-build', workspace: 'w1', versions: opts.versions ?? [] },
  })
  await settle()
}

const q = (id: string, root: ParentNode = document) => root.querySelectorAll(`[data-testid="${id}"]`)
const item = (id: string) =>
  document.querySelector(`[data-testid="corrections-entry"][data-id="${id}"]`) as HTMLElement
const click = (el: Element | null | undefined) => {
  ;(el as HTMLButtonElement).click()
  flushSync()
}
const typeInto = (field: HTMLInputElement | HTMLSelectElement, text: string, event: 'input' | 'change') => {
  field.value = text
  field.dispatchEvent(new Event(event, { bubbles: true }))
  flushSync()
}
const calls = () => fetchMock.mock.calls.map((c) => String(c[0]))

describe('CorrectionsCard', () => {
  it('orders the entries by status and shows the text, the runs and the supervisor note', async () => {
    await render(
      list({
        entries: [
          entry({ correction: { id: 'c-dismissed', status: 'dismissed', kind: 'node_note', text: 'retry twice', runs: 2, via_supervisor: true, node: null, gate: null } }),
          entry({ correction: { id: 'c-fixed', status: 'fixed', text: 'quote the ticket id', runs: 5, via_supervisor: true, fix: FIX }, rate: { base_runs: 2, base_hits: 2, fix_runs: 3, fix_hits: 1 } }),
          entry({ correction: { id: 'c-open', status: 'open', kind: 'rejection', node: null, gate: null, text: 'use the v2 endpoint', runs: 1 } }),
          entry({ correction: { id: 'c-reopened', status: 'reopened', text: 'ask for a plan before writing files', runs: 3 } }),
          entry({ correction: { id: 'c-withheld', status: 'proposed', kind: 'rejection', text: null, node: 'publish-note', gate: null, runs: 2 } }),
          entry({ correction: { id: 'c-proposed', status: 'proposed', kind: 'human_answer', node: 'ask-scope', gate: null, text: 'the scope is the whole repo, not the docs folder', runs: 2 } }),
        ],
      }),
    )
    expect(calls()).toEqual(['/api/playbooks/fix-build/corrections?workspace=w1'])
    const groups = [...q('corrections-group')].map((g) => g.getAttribute('data-group'))
    expect(groups).toEqual(['active', 'once', 'fixed', 'dismissed'])
    expect(item('c-proposed').textContent).toContain('the scope is the whole repo, not the docs folder')
    expect(item('c-proposed').textContent).toContain('2 runs')
    expect(item('c-reopened').textContent).toContain('ask for a plan before writing files')
    expect(item('c-open').textContent).toContain('use the v2 endpoint')
    expect(item('c-withheld').textContent).toContain('text withheld')
    expect(item('c-fixed').textContent).toContain('quote the ticket id')
    expect(item('c-fixed').textContent).toContain('5 runs')
    expect(item('c-fixed').textContent).toContain('via the supervisor channel')
    expect(item('c-dismissed').textContent).toContain('via the supervisor channel')
    // The status word of every entry, including the ones the group title
    // would otherwise blur (a reopened entry sits in the proposed group).
    expect(item('c-proposed').getAttribute('data-status')).toBe('proposed')
    expect(item('c-reopened').textContent).toContain('reopened')
    expect(item('c-open').textContent).toContain('open')
    expect(item('c-fixed').textContent).toContain('fixed')
    expect(item('c-dismissed').textContent).toContain('dismissed')
    // A rejected result keeps its kind and node in the meta line.
    expect(item('c-withheld').textContent).toContain('rejection')
    expect(item('c-withheld').textContent).toContain('publish-note')
  })

  it('shows the draft with its node and the instruction, only where the server offers one', async () => {
    await render(
      list({
        entries: [
          entry({ correction: { id: 'c-proposed' }, draft: { node: 'implement', line: DRAFT_LINE } }),
          entry({ correction: { id: 'c-open', status: 'open', runs: 1 } }),
        ],
      }),
    )
    const draft = q('corrections-draft', item('c-proposed'))[0]
    expect(draft?.textContent).toContain(DRAFT_LINE)
    expect(draft?.textContent).toContain('implement')
    expect(draft?.textContent).toContain("Add this line to the node's prompt")
    expect(draft?.textContent).toContain('the version that carries the fix')
    expect(q('corrections-draft', item('c-open')).length).toBe(0)
  })

  it('copies the draft', async () => {
    const writeText = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue(undefined)
    await render(list({ entries: [entry({ draft: { node: 'implement', line: DRAFT_LINE } })] }))
    click(q('corrections-copy', item('c-x'))[0])
    await settle()
    expect(writeText).toHaveBeenCalledWith(DRAFT_LINE)
    expect(q('corrections-copied').length).toBe(1)
  })

  it('marks fixed with the version after the confirm', async () => {
    await render(
      list({ entries: [entry({ correction: { id: 'c-proposed' }, draft: { node: 'implement', line: DRAFT_LINE } })] }),
      { versions: ['1.1.0', '1.2.0'] },
    )
    const select = q('corrections-version', item('c-proposed'))[0] as HTMLSelectElement
    typeInto(select, '1.2.0', 'change')
    click(q('corrections-fixed', item('c-proposed'))[0])
    // Confirm first: the question names the version, nothing is posted yet.
    expect(q('corrections-fixed-question')[0].textContent).toContain('Mark fixed in 1.2.0?')
    expect(calls()).toEqual(['/api/playbooks/fix-build/corrections?workspace=w1'])
    fetchMock.mockResolvedValueOnce(json({}))
    fetchMock.mockResolvedValueOnce(json(list()))
    click(q('corrections-fixed-confirm', item('c-proposed'))[0])
    await settle()
    expect(calls()).toEqual([
      '/api/playbooks/fix-build/corrections?workspace=w1',
      '/api/playbooks/fix-build/corrections/c-proposed/fixed?workspace=w1',
      '/api/playbooks/fix-build/corrections?workspace=w1',
    ])
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({ version: '1.2.0' })
  })

  it('takes the version from a text input when the page has no version list', async () => {
    await render(list({ entries: [entry({ correction: { id: 'c-proposed' } })] }))
    const input = q('corrections-version', item('c-proposed'))[0] as HTMLInputElement
    typeInto(input, '1.3.0', 'input')
    click(q('corrections-fixed', item('c-proposed'))[0])
    fetchMock.mockResolvedValueOnce(json({}))
    fetchMock.mockResolvedValueOnce(json(list()))
    click(q('corrections-fixed-confirm', item('c-proposed'))[0])
    await settle()
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({ version: '1.3.0' })
  })

  it('dismisses with the reason when one is typed', async () => {
    await render(list({ entries: [entry({ correction: { id: 'c-open', status: 'open', runs: 1 } })] }))
    const reason = q('corrections-reason', item('c-open'))[0] as HTMLInputElement
    typeInto(reason, 'not worth showing', 'input')
    fetchMock.mockResolvedValueOnce(json({}))
    fetchMock.mockResolvedValueOnce(json(list()))
    click(q('corrections-dismiss', item('c-open'))[0])
    await settle()
    expect(calls()[1]).toBe('/api/playbooks/fix-build/corrections/c-open/dismiss?workspace=w1')
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({ reason: 'not worth showing' })
  })

  it('reopens a fixed entry', async () => {
    await render(
      list({
        entries: [
          entry({
            correction: { id: 'c-fixed', status: 'fixed', runs: 5, fix: FIX },
            rate: { base_runs: 2, base_hits: 2, fix_runs: 3, fix_hits: 2 },
          }),
        ],
      }),
    )
    fetchMock.mockResolvedValueOnce(json({}))
    fetchMock.mockResolvedValueOnce(json(list()))
    click(q('corrections-reopen', item('c-fixed'))[0])
    await settle()
    expect(calls()[1]).toBe('/api/playbooks/fix-build/corrections/c-fixed/reopen?workspace=w1')
    expect(JSON.parse(String(fetchMock.mock.calls[1][1]?.body))).toEqual({})
  })

  it('shows both rates of a fixed entry', async () => {
    await render(
      list({
        entries: [
          entry({
            correction: { id: 'c-fixed', status: 'fixed', runs: 5, fix: FIX },
            rate: { base_runs: 2, base_hits: 2, fix_runs: 3, fix_hits: 1 },
          }),
        ],
      }),
    )
    expect(q('corrections-rates')[0].textContent).toBe('1 of 3 runs on 1.2.0; was 2 of 2')
  })

  it('offers only the actions that apply', async () => {
    await render(
      list({
        entries: [
          entry({ correction: { id: 'c-proposed' } }),
          entry({ correction: { id: 'c-open', status: 'open', runs: 1 } }),
          entry({ correction: { id: 'c-fixed', status: 'fixed', runs: 5, fix: FIX }, rate: { base_runs: 2, base_hits: 2, fix_runs: 3, fix_hits: 1 } }),
          entry({ correction: { id: 'c-dismissed', status: 'dismissed', runs: 5 } }),
        ],
      }),
    )
    expect(q('corrections-fixed', item('c-proposed')).length).toBe(1)
    expect(q('corrections-dismiss', item('c-proposed')).length).toBe(1)
    expect(q('corrections-reopen', item('c-proposed')).length).toBe(0)
    expect(q('corrections-fixed', item('c-open')).length).toBe(0)
    expect(q('corrections-dismiss', item('c-open')).length).toBe(1)
    expect(q('corrections-reopen', item('c-open')).length).toBe(0)
    expect(q('corrections-fixed', item('c-fixed')).length).toBe(0)
    expect(q('corrections-dismiss', item('c-fixed')).length).toBe(1)
    expect(q('corrections-reopen', item('c-fixed')).length).toBe(1)
    expect(q('corrections-dismiss', item('c-dismissed')).length).toBe(0)
    expect(q('corrections-reopen', item('c-dismissed')).length).toBe(1)
  })

  it('only counts the entries of a file this installation did not write', async () => {
    await render(list({ signed: false, unsigned_entries: 2 }))
    expect(q('corrections-unsigned')[0].textContent).toContain('not written by this installation')
    expect(q('corrections-unsigned')[0].textContent).toContain('2 entries withheld')
    expect(q('corrections-entry').length).toBe(0)
    expect(q('corrections-empty').length).toBe(0)
  })

  it('says so when there are no corrections', async () => {
    await render(list())
    expect(q('corrections-empty').length).toBe(1)
    expect(q('corrections-entry').length).toBe(0)
  })

  it('shows a refused action on its entry', async () => {
    await render(list({ entries: [entry({ correction: { id: 'c-proposed' } })] }))
    const input = q('corrections-version', item('c-proposed'))[0] as HTMLInputElement
    typeInto(input, '1.2.0', 'input')
    // The first click only opens the confirm row; the POST is the second.
    click(q('corrections-fixed', item('c-proposed'))[0])
    expect(q('corrections-fixed-question').length).toBe(1)
    fetchMock.mockResolvedValueOnce(json({ error: 'not_proposed' }, 409))
    click(q('corrections-fixed-confirm', item('c-proposed'))[0])
    await settle()
    expect(q('corrections-action-error')[0].textContent).toContain('not_proposed')
    // The entry keeps its buttons, so the person can try again.
    expect(q('corrections-fixed-confirm', item('c-proposed')).length).toBe(1)
  })
})
