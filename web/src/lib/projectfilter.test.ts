import { describe, it, expect } from 'vitest'
import { get } from 'svelte/store'
import { render } from 'svelte/server'
import {
  ALL_PROJECTS,
  PROJECT_FILTER_KEY,
  createProjectFilterStore,
  filterByProject,
  filterProfilesByProject,
  projectOptions,
  projectScopeItems,
  type StorageLike,
} from './projectfilter'
import ProjectFilter from './components/ProjectFilter.svelte'

function memoryStorage(initial: Record<string, string> = {}): StorageLike & {
  data: Record<string, string>
} {
  const data = { ...initial }
  return {
    data,
    getItem(k: string) {
      return Object.prototype.hasOwnProperty.call(data, k) ? data[k]! : null
    },
    setItem(k: string, v: string) {
      data[k] = v
    },
  }
}

describe('createProjectFilterStore', () => {
  it('defaults to ALL_PROJECTS when storage is empty', () => {
    const store = createProjectFilterStore(memoryStorage())
    expect(get(store)).toBe(ALL_PROJECTS)
  })

  it('loads a stored value on init', () => {
    const storage = memoryStorage({ [PROJECT_FILTER_KEY]: 'ws-alpha' })
    const store = createProjectFilterStore(storage)
    expect(get(store)).toBe('ws-alpha')
  })

  it('writes the selection to storage under apb.projectFilter', () => {
    const storage = memoryStorage()
    const store = createProjectFilterStore(storage)
    store.set('ws-beta')
    expect(storage.getItem(PROJECT_FILTER_KEY)).toBe('ws-beta')
    expect(storage.getItem('apb.projectFilter')).toBe('ws-beta')
    expect(get(store)).toBe('ws-beta')
  })
})

describe('filterByProject', () => {
  const items = [
    { workspace_id: 'ws-a', id: '1' },
    { workspace_id: 'ws-b', id: '2' },
    { workspace_id: 'ws-a', id: '3' },
  ]

  it('returns all items for ALL_PROJECTS', () => {
    expect(filterByProject(items, ALL_PROJECTS)).toEqual(items)
  })

  it('returns only matching-workspace items for a known id', () => {
    expect(filterByProject(items, 'ws-a')).toEqual([
      { workspace_id: 'ws-a', id: '1' },
      { workspace_id: 'ws-a', id: '3' },
    ])
  })

  it('falls back to all items for an unknown id', () => {
    expect(filterByProject(items, 'ws-gone')).toEqual(items)
  })
})

describe('filterProfilesByProject', () => {
  const items = [
    { workspace_id: 'ws-a', scope: 'project', id: '1' },
    { workspace_id: 'ws-b', scope: 'project', id: '2' },
    { workspace_id: '_', scope: 'global', id: '3' },
  ]

  it('returns all items for ALL_PROJECTS', () => {
    expect(filterProfilesByProject(items, ALL_PROJECTS)).toEqual(items)
  })

  it('keeps global rows and matching-project rows, drops other projects', () => {
    expect(filterProfilesByProject(items, 'ws-a')).toEqual([
      { workspace_id: 'ws-a', scope: 'project', id: '1' },
      { workspace_id: '_', scope: 'global', id: '3' },
    ])
  })

  it('falls back to all items for an unknown project id', () => {
    expect(filterProfilesByProject(items, 'ws-gone')).toEqual(items)
  })
})

describe('projectScopeItems', () => {
  it('drops global rows and keeps project-scope rows, contributing no option for global', () => {
    const items = [
      { workspace_id: 'ws-a', scope: 'project', id: '1' },
      { workspace_id: '_', scope: 'global', id: '2' },
      { workspace_id: 'ws-b', scope: 'project', id: '3' },
    ]
    expect(projectScopeItems(items)).toEqual([
      { workspace_id: 'ws-a', scope: 'project', id: '1' },
      { workspace_id: 'ws-b', scope: 'project', id: '3' },
    ])
  })

  it('returns an empty array when every row is global-scope', () => {
    const items = [
      { workspace_id: '_', scope: 'global', id: '1' },
      { workspace_id: '_', scope: 'global', id: '2' },
    ]
    expect(projectScopeItems(items)).toEqual([])
  })
})

describe('projectOptions', () => {
  it('dedupes by workspace_id, labels with project or this project, sorts by label', () => {
    const opts = projectOptions([
      { workspace_id: 'ws-z', project: 'Zebra' },
      { workspace_id: 'ws-a', project: null },
      { workspace_id: 'ws-z', project: 'Zebra' },
      { workspace_id: 'ws-m', project: 'Middle' },
      { workspace_id: 'ws-a', project: 'ignored-dup' },
    ])
    expect(opts).toEqual([
      { id: 'ws-m', label: 'Middle' },
      { id: 'ws-a', label: 'this project' },
      { id: 'ws-z', label: 'Zebra' },
    ])
  })
})

describe('ProjectFilter', () => {
  it('SSR-renders the All projects option', () => {
    const { body } = render(ProjectFilter, {
      props: {
        items: [{ workspace_id: 'ws-1', project: 'Alpha' }],
      },
    })
    expect(body).toContain('All projects')
  })
})
