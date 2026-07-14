import { describe, expect, it } from 'vitest'
import { formatDiff, type DiffLine } from './difffmt'

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

  it('satisfies DiffLine type shape', () => {
    const line: DiffLine = { kind: 'add', text: 'x' }
    expect(line.kind).toBe('add')
  })
})
