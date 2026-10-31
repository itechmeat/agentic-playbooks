import { describe, it, expect } from 'vitest'
import type { RunList, RunListItem } from './api.gen'
import {
  batchLabel,
  itemText,
  listBadge,
  listChips,
  processedPercent,
  visibleItems,
} from './listprogress'

const item = (id: string, state: string, extra: Partial<RunListItem> = {}): RunListItem => ({
  id,
  state,
  round: 0,
  attempts: 1,
  ...extra,
})

const list = (items: RunListItem[], over: Partial<RunList> = {}): RunList => ({
  node: 'triage',
  total: 43,
  processed: 12,
  passed: 11,
  passed_with_note: 2,
  needs_look: 1,
  pending: 28,
  running: 3,
  batch: 2,
  batches: 5,
  items,
  ...over,
})

describe('listprogress', () => {
  it('counts plain passes apart from passes with a note', () => {
    const chips = Object.fromEntries(listChips(list([])).map((c) => [c.key, c.count]))
    expect(chips).toEqual({ passed: 9, with_note: 2, needs_look: 1, running: 3, pending: 28 })
    expect(processedPercent(list([]))).toBe(27)
    expect(processedPercent(list([], { total: 0, processed: 0 }))).toBe(0)
    expect(processedPercent(list([], { total: 200, processed: 199 }))).toBe(99)
    expect(batchLabel(list([]))).toBe('batch 2 of 5')
    expect(batchLabel(list([], { batch: 0 }))).toBeNull()
  })

  it('puts items that need a look first and maps the note filters onto passed items', () => {
    const items = [
      item('b', 'passed', { verdict: 'accept_with_note', note: 'vague name' }),
      item('a', 'passed', { verdict: 'accept' }),
      item('c', 'needs_look', { reason: 'check_unknown', note: 'no body' }),
      item('d', 'running'),
    ]
    expect(visibleItems(items, 'all', false).rows.map((i) => i.id)).toEqual(['c', 'd', 'a', 'b'])
    expect(visibleItems(items, 'with_note', false).rows.map((i) => i.id)).toEqual(['b'])
    expect(visibleItems(items, 'passed', false).rows.map((i) => i.id)).toEqual(['a'])
    expect(itemText(items[2])).toBe('check unknown: no body')
  })

  it('badges a list node with its progress, and only a list node', () => {
    expect(listBadge({ id: 'x', type: 'agent_task', list: {} }, { processed: 12, total: 43 })).toBe('12/43')
    expect(listBadge({ id: 'x', type: 'agent_task', list: {} })).toBe('list')
    expect(listBadge({ id: 'x', type: 'agent_task' }, { processed: 1, total: 2 })).toBeNull()
  })
})
