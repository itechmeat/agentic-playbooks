import { writable, type Writable } from 'svelte/store'

/** Sentinel value for the "All projects" filter option. */
export const ALL_PROJECTS = 'all'

/** Shared localStorage key for the project filter selection. */
export const PROJECT_FILTER_KEY = 'apb.projectFilter'

/** Minimal storage surface used by the store (real localStorage or a test double). */
export type StorageLike = {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
}

export interface ProjectOption {
  id: string
  label: string
}

function guardedLocalStorage(): StorageLike | undefined {
  if (typeof localStorage !== 'undefined') return localStorage
  return undefined
}

/**
 * Create a project-filter store that persists to the given storage.
 * When `storage` is omitted, uses `localStorage` when available (SSR-safe).
 */
export function createProjectFilterStore(storage?: StorageLike): Writable<string> {
  const backend = storage ?? guardedLocalStorage()
  let initial = ALL_PROJECTS
  if (backend) {
    const raw = backend.getItem(PROJECT_FILTER_KEY)
    if (raw != null) initial = raw
  }

  const inner = writable(initial)

  return {
    subscribe: inner.subscribe,
    set(value: string) {
      if (backend) backend.setItem(PROJECT_FILTER_KEY, value)
      inner.set(value)
    },
    update(fn) {
      inner.update((current) => {
        const next = fn(current)
        if (backend) backend.setItem(PROJECT_FILTER_KEY, next)
        return next
      })
    },
  }
}

/** Shared singleton used by list pages and the ProjectFilter control. */
export const projectFilter = createProjectFilterStore()

/**
 * Unique project options from list items, keyed by workspace_id.
 * Labels use `project || 'this project'`. Sorted by label. Does not include
 * the "All projects" entry.
 */
export function projectOptions<T extends { workspace_id: string; project?: string | null }>(
  items: T[],
): ProjectOption[] {
  const byId = new Map<string, string>()
  for (const item of items) {
    if (!byId.has(item.workspace_id)) {
      byId.set(item.workspace_id, item.project || 'this project')
    }
  }
  return [...byId.entries()]
    .map(([id, label]) => ({ id, label }))
    .sort((a, b) => a.label.localeCompare(b.label))
}

/**
 * Client-side filter by selected workspace_id.
 * Returns all items when selected is ALL_PROJECTS or an unknown id.
 */
export function filterByProject<T extends { workspace_id: string }>(
  items: T[],
  selected: string,
): T[] {
  if (selected === ALL_PROJECTS) return items
  const known = items.some((item) => item.workspace_id === selected)
  if (!known) return items
  return items.filter((item) => item.workspace_id === selected)
}

/**
 * Client-side filter by selected workspace_id for scope-aware rows (profiles).
 * Global-scope rows always pass through, since a global profile applies to
 * every project. Returns all items when selected is ALL_PROJECTS or an
 * unknown id among the project-scope rows - intentionally mirroring
 * `filterByProject`'s existing "unknown id falls back to all items" behavior,
 * scoped here to the project-scope subset rather than every row.
 */
export function filterProfilesByProject<T extends { scope: string; workspace_id: string }>(
  items: T[],
  selected: string,
): T[] {
  if (selected === ALL_PROJECTS) return items
  const known = items.some((item) => item.scope !== 'global' && item.workspace_id === selected)
  if (!known) return items
  return items.filter((item) => item.scope === 'global' || item.workspace_id === selected)
}

/**
 * Rows that carry their own project identity (scope !== 'global'). Used to
 * build the project-filter selector options: global-scope rows have no
 * project of their own and would otherwise inject a bogus workspace option.
 */
export function projectScopeItems<T extends { scope: string }>(items: T[]): T[] {
  return items.filter((item) => item.scope !== 'global')
}
