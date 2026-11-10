import { describe, expect, it } from 'vitest'
import { runHref, startedByLine, starterHref, startedRunLabel } from './startedruns'
import type { StartedBy } from './api.gen'

const by: StartedBy = { run_id: 't-1', node: 'triage', attempt: 2, verified: true, chain: 1 }

describe('startedruns', () => {
  it('names the starter and links it within its workspace', () => {
    expect(startedByLine(by, 'ws-a')).toBe('Started by run t-1 (node triage, attempt 2)')
    expect(starterHref(by, 'ws-a')).toBe('#/run/ws-a/t-1')
    const same = { ...by, workspace_id: 'ws-a' }
    expect(starterHref(same, 'ws-a')).toBe('#/run/ws-a/t-1')
  })

  it('shows a starter in another workspace as text', () => {
    const other = { ...by, workspace_id: 'ws-b', verified: false }
    expect(startedByLine(other, 'ws-a')).toBe(
      'Started by run t-1 (node triage, attempt 2), not verified, in workspace ws-b',
    )
    expect(starterHref(other, 'ws-a')).toBeNull()
  })

  it('labels and links a started run', () => {
    expect(startedRunLabel({ run_id: 'f-1', playbook: 'fix', scope: 'project' })).toBe('f-1 (fix)')
    expect(runHref('ws a', 'f/1')).toBe('#/run/ws%20a/f%2F1')
  })
})
