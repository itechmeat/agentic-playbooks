// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from 'vitest'
import { mount, unmount } from 'svelte'
import EvidenceClaims from './EvidenceClaims.svelte'
import {
  STRUCTURALLY_VALID_WORDING,
  UNSUPPORTED_WORDING,
  collectorLine,
  type EvidenceRow,
} from './results'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const ref = `r:${'a'.repeat(64)}/file:workspace:mail.jsonl@m1`
const row: EvidenceRow = {
  node: 'read',
  attempt_number: 2,
  attempt: 14,
  session: 'f'.repeat(32),
  closed: true,
  read_chars: 120,
  budget_chars: 200000,
  snapshots: ['a'.repeat(64)],
  views: [
    { id: 'collect/file:workspace:mail.jsonl', kind: 'records', complete: true, collector_complete: true, records: 2 },
    { id: 'collect/output', kind: 'output', complete: true, records: 1 },
  ],
  checked: true,
  output_unsupported: false,
  problems: [],
  claims: [
    {
      id: 'c1',
      verdict: 'structurally_valid',
      wording: STRUCTURALLY_VALID_WORDING,
      text: 'The March invoice was paid.',
      problems: [],
      flags: ['short_span'],
      cites: [{ ref: `${ref}#[4,11)`, fragment: '<b>invoice</b>' }],
    },
    {
      id: 'c2',
      verdict: 'unsupported',
      wording: UNSUPPORTED_WORDING,
      text: 'The printer is jammed.',
      problems: ['`r:x` was only seen in a listing or a search: read it before citing it'],
      flags: [],
      cites: [],
      absence: ['complaint', 'refund'],
    },
  ],
}

describe('EvidenceClaims', () => {
  it('shows each claim with the fixed wording, its flags and its fragment as plain text', () => {
    app = mount(EvidenceClaims, { target: document.body, props: { rows: [row] } })
    const claims = document.querySelectorAll('[data-testid="evidence-claim"]')
    expect(claims.length).toBe(2)
    // Unsupported claims come first and are never shown as a fact.
    expect(claims[0].querySelector('[data-testid="evidence-verdict"]')?.textContent).toBe('Not supported')
    expect(claims[0].querySelector('[data-testid="evidence-wording"]')?.textContent).toBe(UNSUPPORTED_WORDING)
    expect(claims[0].textContent).toContain('only seen in a listing or a search')
    const valid = claims[1]
    expect(valid.querySelector('[data-testid="evidence-wording"]')?.textContent).toBe(STRUCTURALLY_VALID_WORDING)
    expect(valid.querySelector('[data-testid="evidence-flags"]')?.textContent).toContain('Worth a look: short span')
    const fragment = valid.querySelector('[data-testid="evidence-fragment"]')
    expect(fragment?.textContent).toBe('<b>invoice</b>')
    expect(fragment?.querySelector('b')).toBeNull()
    expect(document.body.textContent).not.toMatch(/confirmed|verified/i)
    expect(document.querySelector('[data-testid="evidence-collector"]')?.textContent).toContain(
      'the collector says the package is complete',
    )
    expect(document.body.textContent).toContain('read 120 of 200000 characters')
    // An absence claim says what it searched.
    const absence = document.querySelector('[data-testid="evidence-absence"]')
    expect(absence?.textContent).toContain('complaint, refund')
  })
})

describe('evidence helpers', () => {
  it('words the collector statement', () => {
    expect(collectorLine(row.views[1])).toBeNull()
    expect(collectorLine({ ...row.views[0], collector_complete: false, complete: false })).toBe(
      'collect/file:workspace:mail.jsonl: the collector says the package is not complete; incomplete as stored',
    )
  })
})
