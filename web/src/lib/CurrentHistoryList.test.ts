// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import CurrentHistoryList from './CurrentHistoryList.svelte'
import type { History } from './api.gen'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`)
const entries = () =>
  [...document.querySelectorAll('[data-testid="current-history-entry"]')].map((li) =>
    li.textContent?.replace(/\s+/g, ' ').trim(),
  )

function show(history: History | null) {
  app = mount(CurrentHistoryList, { target: document.body, props: { history } })
  flushSync()
}

describe('CurrentHistoryList', () => {
  it('lists the moves newest first with who, when and why', () => {
    show({
      entries: [
        {
          change_id: 'c2',
          at_ms: Date.UTC(2026, 9, 31, 12, 5),
          from: '1.1.0',
          to: '1.0.0',
          by: { kind: 'person', surface: 'dashboard' },
          reason: '<b>roll back</b>',
          superseded_candidate: '1.1.1',
        },
        { kind: 'aborted', change_id: 'abcdef0123456789', at_ms: Date.UTC(2026, 9, 30) },
        {
          change_id: 'c1',
          at_ms: Date.UTC(2026, 9, 29),
          from: '1.0.0',
          to: '1.1.0',
          by: { kind: 'candidate_promotion', run_id: 'r7', basis: 'successes:3' },
        },
      ],
      incomplete: false,
      pending: false,
    })
    expect(entries()).toEqual([
      '1.1.0 -> 1.0.0 2026-10-31 12:05 a person on the dashboard <b>roll back</b> candidate 1.1.1 superseded',
      'aborted move abcdef01 2026-10-30 00:00',
      '1.0.0 -> 1.1.0 2026-10-29 00:00 candidate promotion (successes:3, run r7)',
    ])
    // A reason is plain text, never markup.
    expect(document.querySelector('[data-testid="current-history"] b')).toBeNull()
    expect(byTestId('current-history-incomplete')).toBeNull()
    expect(byTestId('current-history-pending')).toBeNull()
  })

  it('says when the record has a gap or a move waits', () => {
    show({ entries: [], incomplete: true, pending: true })
    expect(byTestId('current-history')?.textContent).toContain('no recorded moves')
    expect(byTestId('current-history-incomplete')?.textContent).toContain('the record has a gap')
    expect(byTestId('current-history-pending')?.textContent).toContain('interrupted')
  })
})
