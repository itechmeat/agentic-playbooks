import { describe, expect, it } from 'vitest'
import { diffEdgeChanges, diffOrderChanges, formatDiff } from './difffmt'

describe('formatDiff', () => {
  it('classifies add/del/context lines', () => {
    const diff = [
      '--- a',
      '+++ b',
      '@@ -1,2 +1,3 @@',
      ' same',
      '-removed',
      '+added',
      ' ctx2',
    ].join('\n')
    const lines = formatDiff(diff)
    expect(lines.map((l) => l.kind)).toEqual(['meta', 'meta', 'meta', 'ctx', 'del', 'add', 'ctx'])
    expect(lines[4]).toEqual({ kind: 'del', text: 'removed' })
    expect(lines[5]).toEqual({ kind: 'add', text: 'added' })
  })

  it('keeps full raw text on meta lines without the prefix', () => {
    const lines = formatDiff('@@ -1,1 +1,1 @@')
    expect(lines[0]).toEqual({ kind: 'meta', text: '@@ -1,1 +1,1 @@' })
  })

  it('strips the leading +/-/space marker from content lines', () => {
    const lines = formatDiff(' keep\n-gone\n+new')
    expect(lines[0].text).toBe('keep')
    expect(lines[1].text).toBe('gone')
    expect(lines[2].text).toBe('new')
  })

  it('handles empty input', () => {
    expect(formatDiff('')).toEqual([])
    expect(formatDiff('')).toHaveLength(0)
  })

  it('treats plain non-diff lines as context', () => {
    const lines = formatDiff('just text')
    expect(lines[0]).toEqual({ kind: 'ctx', text: 'just text' })
  })
})

describe('diff edge lists', () => {
  const d = (from: string, to: string, ordinal: number) => ({ kind: 'derived' as const, from, to, ordinal })

  it('lists only the edges whose attributes changed, before and after', () => {
    const rows = diffEdgeChanges({
      edges: [
        { before: d('a', 'b', 1), after: d('a', 'b', 1), changed: [] },
        { before: d('a', 'c', 2), after: { kind: 'authored', id: 'retry' }, changed: ['id', 'condition'] },
        { before: null, after: d('a', 'd', 1), changed: [] },
      ],
    })
    expect(rows).toEqual([{ before: 'e:a>c#2', after: 'id:retry', changed: ['id', 'condition'] }])
  })

  it('lists the sources whose edges are checked in another order', () => {
    const rows = diffOrderChanges({
      edge_order_changed: [{ node: 'gate', before: [d('gate', 'x', 1), d('gate', 'y', 1)], after: [d('gate', 'y', 1), d('gate', 'x', 1)] }],
    })
    expect(rows).toEqual([{ node: 'gate', before: ['e:gate>x#1', 'e:gate>y#1'], after: ['e:gate>y#1', 'e:gate>x#1'] }])
  })

  it('lists nothing for a diff from an older server', () => {
    expect(diffEdgeChanges({})).toEqual([])
    expect(diffOrderChanges(null)).toEqual([])
  })
})
