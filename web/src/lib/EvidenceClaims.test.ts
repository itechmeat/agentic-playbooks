// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import EvidenceClaims from './EvidenceClaims.svelte'
import { EVIDENCE_VERDICT_TEXTS } from './consts.gen'
import { flagLine, type SessionShown } from './results'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const snap = 'a'.repeat(64)
const session: SessionShown = {
  node: 'use',
  attempt: 1,
  attempt_id: 12,
  state: 'checked',
  sources: [{ node: 'collect', execution: 1, snapshot_id: snap }],
  packages: [
    {
      view: 'collect.1/file:workspace:mail.jsonl',
      records: 2,
      complete: true,
      collector: 'the collector says the package is complete',
    },
  ],
  trace: { budget_chars: 200000, read_chars: 97 },
  problems: [],
  claims: [
    {
      id: 'c1',
      text: 'The October invoice is late',
      verdict: 'structurally_valid',
      wording: EVIDENCE_VERDICT_TEXTS.structurally_valid,
      problems: [],
      flags: ['short_span'],
      cites: [{ ref: `r:${snap}/file:workspace:mail.jsonl@m-1`, span: [4, 12], fragment: '<script>x</script>' }],
    },
    {
      id: 'c2',
      text: 'A refund was promised',
      verdict: 'unsupported',
      wording: EVIDENCE_VERDICT_TEXTS.unsupported,
      problems: ['`r:x` was only seen in a listing or a search: read it before citing it'],
      flags: [],
      cites: [{ ref: `r:${snap}/file:workspace:mail.jsonl@m-2` }],
    },
  ],
}

describe('EvidenceClaims', () => {
  it('shows each claim with its fixed wording, fragment and flags', () => {
    app = mount(EvidenceClaims, { target: document.body, props: { sessions: [session] } })
    flushSync()
    const claims = document.querySelectorAll('[data-testid="evidence-claim"]')
    expect(claims).toHaveLength(2)
    const verdicts = [...document.querySelectorAll('[data-testid="evidence-verdict"]')].map((e) => e.textContent)
    expect(verdicts).toEqual([
      'Structurally valid: the cited record exists, was returned whole to this attempt, is of an allowed kind, and the span lies inside it. Whether it supports the claim was not checked.',
      'not supported by a cited record (not shown as a fact)',
    ])
    // A fragment is plain text, never markup.
    const fragment = document.querySelector('[data-testid="evidence-fragment"]')
    expect(fragment?.textContent).toBe('<script>x</script>')
    expect(document.querySelector('script')).toBeNull()
    expect(document.querySelector('[data-testid="evidence-flags"]')?.textContent).toBe(
      'worth a look: the cited span is under 16 characters',
    )
    expect(document.body.textContent).toContain('the collector says the package is complete')
    expect(document.body.textContent).not.toMatch(/confirmed|verified/i)
  })

  it('renders nothing without sessions', () => {
    app = mount(EvidenceClaims, { target: document.body, props: { sessions: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="evidence-claims"]')).toBeNull()
  })

  it('words every flag and none as an error', () => {
    expect(flagLine([])).toBe('')
    const line = flagLine(['whole_record', 'self_citation', 'shared_record', 'no_shared_words'])
    expect(line.startsWith('worth a look: ')).toBe(true)
    expect(line).not.toMatch(/error/i)
  })
})
