// @vitest-environment happy-dom
import { describe, it, expect, afterEach } from 'vitest'
import { flushSync, mount, unmount } from 'svelte'
import type { RunList, RunListItem } from './api.gen'
import ListProgressPanel from './ListProgressPanel.svelte'
import { ITEM_ROW_CAP } from './listprogress'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

const items = (n: number, state: string): RunListItem[] =>
  Array.from({ length: n }, (_, i) => ({ id: `${state}-${String(i).padStart(4, '0')}`, state, round: 0, attempts: 1 }))

function triage(all: RunListItem[]): RunList {
  const count = (s: string) => all.filter((i) => i.state === s).length
  return {
    node: 'triage',
    title: 'Triage',
    total: all.length,
    processed: count('passed') + count('needs_look'),
    passed: count('passed'),
    passed_with_note: 0,
    needs_look: count('needs_look'),
    pending: count('pending'),
    running: count('running'),
    batch: 1,
    batches: 3,
    items: all,
  }
}

const rows = () => document.querySelectorAll('[data-testid="run-list-item"]')

describe('ListProgressPanel', () => {
  it('shows the counts and filters the item table by state', () => {
    const list = triage([...items(3, 'passed'), ...items(1, 'needs_look'), ...items(2, 'running')])
    app = mount(ListProgressPanel, { target: document.body, props: { lists: [list] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-list-processed"]')?.textContent).toBe('4 of 6 processed')
    expect(document.querySelector('[data-testid="run-list-batch"]')?.textContent).toBe('batch 1 of 3')
    const chip = document.querySelector('[data-key="needs_look"]')?.textContent ?? ''
    expect(chip).toContain('1')
    expect(rows().length).toBe(6)
    expect(rows()[0].textContent).toContain('needs a look')
    const select = document.querySelector('[data-testid="run-list-filter"]') as HTMLSelectElement
    select.value = 'running'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    expect(rows().length).toBe(2)
  })

  it('shows the first rows of a long list until asked for all', () => {
    app = mount(ListProgressPanel, { target: document.body, props: { lists: [triage(items(ITEM_ROW_CAP + 3, 'pending'))] } })
    flushSync()
    expect(rows().length).toBe(ITEM_ROW_CAP)
    const more = document.querySelector('[data-testid="run-list-show-all"]') as HTMLButtonElement
    expect(more.textContent).toContain('3 more')
    more.click()
    flushSync()
    expect(rows().length).toBe(ITEM_ROW_CAP + 3)
  })

  it('counts the pending items that have no row yet', () => {
    const list = { ...triage([...items(1, 'passed'), ...items(1, 'pending')]), total: 5, pending: 4 }
    app = mount(ListProgressPanel, { target: document.body, props: { lists: [list] } })
    flushSync()
    expect(document.querySelector('[data-testid="run-list-unlisted"]')?.textContent).toBe('3 pending not started yet')
    const select = document.querySelector('[data-testid="run-list-filter"]') as HTMLSelectElement
    select.value = 'passed'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    flushSync()
    expect(document.querySelector('[data-testid="run-list-unlisted"]')).toBeNull()
  })

  it('renders nothing for a run without a list', () => {
    app = mount(ListProgressPanel, { target: document.body, props: { lists: undefined } })
    flushSync()
    expect(document.querySelector('[data-testid="run-list"]')).toBeNull()
  })
})
