// @vitest-environment happy-dom
// The run's drive notes render the deadline line from the run detail's
// `deadline` block, and nothing for a run without one.
import { afterEach, describe, expect, it } from 'vitest'
import { mount, unmount } from 'svelte'
import RunDriveNotes from './RunDriveNotes.svelte'

let app: ReturnType<typeof mount> | null = null
afterEach(() => {
  if (app) unmount(app)
  app = null
  document.body.innerHTML = ''
})

describe('RunDriveNotes', () => {
  it('shows the deadline line', () => {
    app = mount(RunDriveNotes, {
      target: document.body,
      props: {
        deadline: {
          total_ms: 45 * 60_000,
          elapsed_ms: 33 * 60_000,
          remaining_ms: 12 * 60_000,
          source: 'playbook',
          frozen: false,
          ends_at: '2026-11-10T10:00:00Z',
        },
      },
    })
    const line = document.querySelector('[data-testid="run-deadline"]')
    expect(line?.textContent?.trim()).toBe('Deadline 45m, 12m left (ends 2026-11-10T10:00:00Z)')
  })

  it('shows nothing for a run without a deadline', () => {
    app = mount(RunDriveNotes, { target: document.body, props: {} })
    expect(document.querySelector('[data-testid="run-deadline"]')).toBeNull()
  })
})

describe('RunDriveNotes started runs', () => {
  it('links the starter and the started runs of this workspace', () => {
    app = mount(RunDriveNotes, {
      target: document.body,
      props: {
        workspace: 'ws-a',
        startedBy: { run_id: 't-1', node: 'triage', attempt: 2, verified: true, chain: 1 },
        startedRuns: [{ run_id: 'f-1', playbook: 'fix', scope: 'project' }],
      },
    })
    const by = document.querySelector('[data-testid="run-started-by"] a')
    expect(by?.getAttribute('href')).toBe('#/run/ws-a/t-1')
    expect(by?.textContent).toBe('Started by run t-1 (node triage, attempt 2)')
    const run = document.querySelector('[data-testid="run-started-runs"] a')
    expect(run?.getAttribute('href')).toBe('#/run/ws-a/f-1')
    expect(document.querySelector('[data-testid="run-started-runs-truncated"]')).toBeNull()
  })

  it('shows a starter in another workspace as text, not a link', () => {
    app = mount(RunDriveNotes, {
      target: document.body,
      props: {
        workspace: 'ws-b',
        startedBy: { run_id: 't-1', workspace_id: 'ws-a', verified: true, chain: 1 },
      },
    })
    const line = document.querySelector('[data-testid="run-started-by"]')
    expect(line?.querySelector('a')).toBeNull()
    expect(line?.textContent?.trim()).toBe('Started by run t-1, in workspace ws-a')
  })

  it('says when the list may be incomplete', () => {
    app = mount(RunDriveNotes, {
      target: document.body,
      props: { workspace: 'ws-a', startedRuns: [], startedRunsTruncated: true },
    })
    expect(
      document.querySelector('[data-testid="run-started-runs-truncated"]')?.textContent,
    ).toContain('more may exist beyond the newest 500 runs')
  })
})

describe('RunDriveNotes start key', () => {
  it('says the run was started under a start key', () => {
    app = mount(RunDriveNotes, { target: document.body, props: { startKey: 'a1b2c3d4e5f6' } })
    expect(document.querySelector('[data-testid="run-start-key"]')?.textContent?.trim()).toBe(
      'Started under a start key (a1b2c3d4e5f6).',
    )
  })

  it('says nothing without one', () => {
    app = mount(RunDriveNotes, { target: document.body, props: {} })
    expect(document.querySelector('[data-testid="run-start-key"]')).toBeNull()
  })
})
