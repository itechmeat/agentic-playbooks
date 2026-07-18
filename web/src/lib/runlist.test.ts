import { describe, expect, it } from 'vitest'
import { groupRunsByParent, type RunLike } from './runlist'

function r(run_id: string, parent_run?: string | null): RunLike {
  return { run_id, parent_run }
}

describe('groupRunsByParent', () => {
  it('passes through a list with no parent/child relations unchanged', () => {
    const runs = [r('a'), r('b'), r('c')]
    expect(groupRunsByParent(runs)).toEqual(runs)
  })

  it('moves a child to sit immediately after its parent (reverse-chronological input)', () => {
    // newest-first: child appears before its parent in the raw list.
    const child = r('child-1', 'parent-1')
    const parent = r('parent-1')
    const other = r('other-1')
    const out = groupRunsByParent([child, parent, other])
    expect(out.map((x) => x.run_id)).toEqual(['parent-1', 'child-1', 'other-1'])
  })

  it('keeps multiple children of the same parent in their existing relative order', () => {
    const c2 = r('c2', 'p')
    const c1 = r('c1', 'p')
    const parent = r('p')
    // c2 happens to be newer (listed first) than c1.
    const out = groupRunsByParent([c2, c1, parent])
    expect(out.map((x) => x.run_id)).toEqual(['p', 'c2', 'c1'])
  })

  it('keeps unrelated top-level rows in their original relative order around a parent', () => {
    const child = r('child-1', 'parent-1')
    const between = r('between')
    const parent = r('parent-1')
    const out = groupRunsByParent([child, between, parent])
    // `between` was listed before `parent` in the raw order and has no
    // parent/child relation - it keeps that relative position.
    expect(out.map((x) => x.run_id)).toEqual(['between', 'parent-1', 'child-1'])
  })

  it('treats an orphan (parent_run set but parent absent from the list) as top-level, at its original slot', () => {
    const orphan = r('orphan', 'missing-parent')
    const a = r('a')
    const out = groupRunsByParent([a, orphan])
    expect(out.map((x) => x.run_id)).toEqual(['a', 'orphan'])
  })

  it('supports nested parent/child/grandchild chains', () => {
    const grandchild = r('gc', 'child')
    const child = r('child', 'parent')
    const parent = r('parent')
    const out = groupRunsByParent([grandchild, child, parent])
    expect(out.map((x) => x.run_id)).toEqual(['parent', 'child', 'gc'])
  })

  it('does not mutate the input array', () => {
    const runs = [r('child', 'parent'), r('parent')]
    const before = [...runs]
    groupRunsByParent(runs)
    expect(runs).toEqual(before)
  })
})
