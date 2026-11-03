import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import { nestProjects } from './projectnest'
import ProjectLabel from './components/ProjectLabel.svelte'
import type { Project } from './types'

const project = (workspace_id: string, name: string, parent?: string): Project => ({
  workspace_id,
  name,
  path: `/p/${name}`,
  playbook_count: 1,
  parent_workspace_id: parent ?? null,
})

describe('nestProjects', () => {
  it('lists main projects sorted, each followed by its worktrees', () => {
    const rows = nestProjects([
      project('w2', 'b-feature', 'b'),
      project('a', 'alpha'),
      project('o', 'orphan', 'missing'),
      project('b', 'beta'),
      project('w1', 'a-fix', 'b'),
    ])
    expect(rows.map(({ row, depth }) => [row.workspace_id, depth])).toEqual([
      ['a', 0],
      ['b', 0],
      ['w1', 1],
      ['w2', 1],
      ['o', 0],
    ])
  })

  it('keeps a project that names itself as parent at the top level', () => {
    expect(nestProjects([project('a', 'alpha', 'a')])).toEqual([
      { row: project('a', 'alpha', 'a'), depth: 0 },
    ])
  })

  it('sorts the leftovers of a chain and of a cycle by label at the top level', () => {
    // beta > gamma is a chain deeper than two, and xray and yankee point at
    // each other, so none of them nests and all land at depth 0.
    const rows = nestProjects([
      project('b', 'beta', 'a'),
      project('y', 'yankee', 'x'),
      project('a', 'alpha'),
      project('c', 'gamma', 'b'),
      project('x', 'xray', 'y'),
    ])
    expect(rows.map(({ row, depth }) => [row.workspace_id, depth])).toEqual([
      ['a', 0],
      ['b', 1],
      ['c', 0],
      ['x', 0],
      ['y', 0],
    ])
  })
})

describe('ProjectLabel', () => {
  it('indents a worktree and not a main project', () => {
    expect(render(ProjectLabel, { props: { name: 'wt', depth: 1 } }).body).toContain('worktree')
    expect(render(ProjectLabel, { props: { name: 'main' } }).body).not.toContain('worktree')
  })
})

describe('picker preselection', () => {
  it('the first row shown is not the first project as stored', () => {
    // A worktree may come first in the raw list, but the picker shows the
    // sorted mains first, so a picker that preselects `rows[0]` preselects
    // a row a person never sees first.
    const rows = [
      project('w2', 'b-feature', 'b'),
      project('a', 'alpha'),
      project('b', 'beta'),
    ]
    expect(rows[0].workspace_id).toBe('w2')
    expect(nestProjects(rows)[0].row.workspace_id).toBe('a')
  })
})
