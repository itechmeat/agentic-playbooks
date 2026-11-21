import { describe, it, expect } from 'vitest'
import type { PilotView, RunList, RunListItem } from './api.gen'
import {
  batchLabel,
  itemText,
  listBadge,
  listChips,
  pilotLine,
  processedPercent,
  skipLabel,
  skippedLabel,
  stoppedLine,
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

  it('the label names skipped items', () => {
    const skip = { skipped: 3, read_items: 4, source: 'lst-r0', truncated: false, not_inherited: 0, reworked: 0 }
    expect(skippedLabel(list([], { skip }))).toBe('3 skipped')
    expect(skipLabel(list([], { skip }))).toBe('skip: 4 items from run lst-r0')
    expect(skipLabel(list([], { skip: { ...skip, read_items: 1, truncated: true } }))).toBe(
      'skip: 1 item from run lst-r0, more not kept',
    )
    expect(skipLabel(list([], { skip: { ...skip, source: undefined } }))).toBe('skip: no earlier success')
    expect(skipLabel(list([], { skip: { ...skip, refused: 'skip_supervisor_note: a note' } }))).toBe(
      'skip refused: skip_supervisor_note: a note',
    )
    expect(skippedLabel(list([], { skip: { ...skip, skipped: 0 } }))).toBeNull()
    expect(
      skipLabel(list([], { skip: { ...skip, not_inherited: 2, stopped_stamping: 'skip_scripts_diverged: changed' } })),
    ).toBe('skip: 4 items from run lst-r0, 2 not skipped (output could not be carried); stamping stopped: skip_scripts_diverged: changed')
    expect(
      skipLabel(list([], { skip: { ...skip, source: undefined, stopped_stamping: 'skip_scripts_diverged: changed' } })),
    ).toBe('skip: no earlier success; stamping stopped: skip_scripts_diverged: changed')
    expect(skipLabel(list([], { skip: { ...skip, reworked: 1 } }))).toBe(
      'skip: 4 items from run lst-r0, 1 skipped item taken back because its skip no longer held',
    )
    expect(skipLabel(list([], { skip: { ...skip, reworked: 2 } }))).toBe(
      'skip: 4 items from run lst-r0, 2 skipped items taken back because their skip no longer held',
    )
    expect(skipLabel(list([]))).toBeNull()
    expect(itemText(item('a', 'passed', { skipped_from: 'lst-r0' }))).toBe('skipped (passed in lst-r0)')
  })

  it('badges a list node with its progress, and only a list node', () => {
    expect(listBadge({ id: 'x', type: 'agent_task', list: {} }, { processed: 12, total: 43 })).toBe('12/43')
    expect(listBadge({ id: 'x', type: 'agent_task', list: {} })).toBe('list')
    expect(listBadge({ id: 'x', type: 'agent_task' }, { processed: 1, total: 2 })).toBeNull()
  })

  it('pilotLine names the round and the counts', () => {
    const waiting: PilotView = { round: 1, items: ['i1', 'i2', 'i3'], passed: 2, needs_look: 1, rest: 2, state: 'waiting' }
    expect(pilotLine(waiting)).toBe(
      'pilot round 1: 3 items, 2 passed, 1 needs a look, 2 items left, waiting for a decision',
    )
    expect(pilotLine({ ...waiting, state: 'continued' })).toBeNull()
    expect(pilotLine({ ...waiting, needs_look: 0 })).toBe(
      'pilot round 1: 3 items, 2 passed, 0 need a look, 2 items left, waiting for a decision',
    )
    expect(pilotLine({ ...waiting, items: ['i1'], passed: 1, needs_look: 0, rest: 1 })).toBe(
      'pilot round 1: 1 item, 1 passed, 0 need a look, 1 item left, waiting for a decision',
    )
  })

  it('stoppedLine shows the detail', () => {
    expect(stoppedLine('2 of 2 settled items need a look')).toBe('stopped early: 2 of 2 settled items need a look')
    expect(stoppedLine(null)).toBeNull()
  })
})
