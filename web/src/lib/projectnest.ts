import type { Project } from './types'

/** A row of a project picker: the row and how deep it is nested. */
export interface Nested<T> {
  row: T
  /** 0 for a main project, 1 for a git linked worktree under its parent. */
  depth: number
}

/**
 * Rows ordered for a project picker, the same rule `apb projects list`
 * follows: each main project sorted by label, followed by the worktrees whose
 * parent it is (depth 1), also sorted. A worktree whose parent is not among
 * `rows` is listed as a main project.
 */
export function nestByParent<T>(
  rows: T[],
  id: (row: T) => string,
  parent: (row: T) => string | null | undefined,
  label: (row: T) => string,
): Nested<T>[] {
  const ids = new Set(rows.map(id))
  const nestedUnder = (row: T): string | null => {
    const p = parent(row)
    return p && p !== id(row) && ids.has(p) ? p : null
  }
  const byLabel = (a: T, b: T) => label(a).localeCompare(label(b))
  const mains = rows.filter((r) => nestedUnder(r) === null).sort(byLabel)
  const children = new Map<string, T[]>()
  for (const r of rows) {
    const p = nestedUnder(r)
    if (p !== null) children.set(p, [...(children.get(p) ?? []), r])
  }
  const out: Nested<T>[] = []
  for (const m of mains) {
    out.push({ row: m, depth: 0 })
    for (const c of (children.get(id(m)) ?? []).sort(byLabel)) out.push({ row: c, depth: 1 })
    children.delete(id(m))
  }
  // A parent that is itself nested, or a cycle, leaves rows without a main
  // to sit under; they stay at depth 0, sorted by label like every list.
  const leftovers = [...children.values()].flat().sort(byLabel)
  for (const r of leftovers) out.push({ row: r, depth: 0 })
  return out
}

/** Registered projects ordered with worktrees nested under their parent. */
export function nestProjects(projects: Project[]): Nested<Project>[] {
  return nestByParent(
    projects,
    (p) => p.workspace_id,
    (p) => p.parent_workspace_id,
    (p) => p.name,
  )
}
