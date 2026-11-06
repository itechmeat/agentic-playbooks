// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import AttemptIsolationPanel from './AttemptIsolationPanel.svelte'
import { ISOLATION_CODE_TEXTS, ISOLATION_REASON_TEXTS } from './consts.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('AttemptIsolationPanel', () => {
  it('shows a badge per attempt with its limits and why a sandbox was missing', () => {
    const rows = [
      {
        node: 'build',
        attempt: 1,
        isolation: { level: 'full', backend: 'bwrap', network: 'none', holds: [], limits: ['user.host_uid', 'kernel.escapes'], view: [] },
      },
      {
        node: 'lint',
        attempt: 2,
        isolation: {
          level: 'best_effort',
          backend: 'none',
          network: 'open',
          holds: [],
          limits: ['no_sandbox'],
          fallbacks: ['disabled (APB_SANDBOX=off)'],
          view: [],
        },
      },
    ]
    app = mount(AttemptIsolationPanel, { target: document.body, props: { rows } })
    flushSync()
    const badges = [...document.querySelectorAll('[data-testid="run-isolation-badge"]')].map((b) => b.textContent?.trim())
    expect(badges).toEqual(['full · sandbox · network none', 'best_effort · no sandbox · network open'])
    const limits = [...document.querySelectorAll('[data-testid="run-isolation-limit"]')].map((l) => l.textContent ?? '')
    expect(limits.length).toBe(3)
    expect(limits[0]).toContain(ISOLATION_CODE_TEXTS['user.host_uid'])
    const text = document.body.textContent ?? ''
    expect(text).toContain(ISOLATION_REASON_TEXTS.disabled)
    expect(text).toContain('#2')
  })

  it("shows each read-only check on its attempt's row, a script's check on its own, and the changes that failed it", () => {
    const rows = [
      {
        node: 'look',
        attempt: 1,
        isolation: { level: 'full', backend: 'bwrap', network: 'none', holds: [], limits: [], view: [] },
      },
    ]
    const readOnly = [
      { node: 'look', attempt: 1, tier: 'enforced' as const },
      {
        node: 'scan',
        attempt: 2,
        tier: 'detected_partial' as const,
        entries: 20000,
        bytes: 4096,
        partial: 'entries' as const,
        violated: { changes: [{ path: 'notes.md', change: 'modified' }], truncated: 3, evidence: 'read_only/scan/2.json' },
      },
      { node: 'sh', attempt: 1, tier: 'detected' as const, violated: { changes: [{ path: '.env', change: 'added' }] } },
    ]
    app = mount(AttemptIsolationPanel, { target: document.body, props: { rows, readOnly } })
    flushSync()
    expect(document.querySelectorAll('[data-testid="run-isolation-row"]').length).toBe(3)
    const badges = [...document.querySelectorAll('[data-testid="run-read-only-badge"]')]
    expect(badges.map((b) => b.textContent?.trim())).toEqual([
      'read-only · enforced',
      'read-only · detected, partial (entries limit)',
      'read-only · detected',
    ])
    expect(badges.map((b) => b.classList.contains('bare'))).toEqual([false, true, true])
    // The enforced check sits on the isolated attempt's own row.
    const first = document.querySelectorAll('[data-testid="run-isolation-row"]')[0]
    expect(first.querySelector('[data-testid="run-isolation-badge"]')).not.toBeNull()
    expect(first.querySelector('[data-testid="run-read-only-badge"]')).not.toBeNull()
    const changes = [...document.querySelectorAll('[data-testid="run-read-only-change"]')].map((c) => c.textContent?.trim())
    expect(changes).toEqual(['notes.md modified', '.env added'])
    const text = document.body.textContent ?? ''
    expect(text).toContain('3 more')
    expect(text).toContain('read_only/scan/2.json')
    expect(text).toContain('read_only_dirty')
    expect(text).toContain('over 20000 entries, 4096 bytes')
  })

  it('counts one entry and one byte in the singular', () => {
    app = mount(AttemptIsolationPanel, {
      target: document.body,
      props: { rows: [], readOnly: [{ node: 'one', attempt: 1, tier: 'detected', entries: 1, bytes: 1 }] },
    })
    flushSync()
    expect(document.body.textContent).toContain('over 1 entry, 1 byte')
  })

  it("names the batch members a violation ran beside, and the plain wording when there were none", () => {
    const readOnly = [
      {
        node: 'scan',
        attempt: 1,
        tier: 'detected' as const,
        violated: {
          changes: [{ path: 'a.txt', change: 'modified' }],
          concurrent_with: ['write', 'other'],
        },
      },
      {
        node: 'solo',
        attempt: 1,
        tier: 'detected' as const,
        violated: { changes: [{ path: 'b.txt', change: 'added' }] },
      },
    ]
    app = mount(AttemptIsolationPanel, { target: document.body, props: { readOnly } })
    flushSync()
    const violated = [...document.querySelectorAll('[data-testid="run-read-only-violated"]')].map(
      (v) => v.textContent?.trim(),
    )
    expect(violated).toHaveLength(2)
    expect(violated[0]).toContain(
      'The tree changed while this node ran beside write, other; the change cannot be attributed to one of them',
    )
    expect(violated[0]).toContain('read_only_dirty')
    expect(violated[1]).toContain('The node changed the tree')
  })

  it('renders nothing for a run without an isolated attempt', () => {
    app = mount(AttemptIsolationPanel, { target: document.body, props: { rows: [] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-isolation"]')).toBeNull()
  })
})
