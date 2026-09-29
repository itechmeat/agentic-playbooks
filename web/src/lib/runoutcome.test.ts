import { describe, expect, it } from 'vitest'
import { runEventJournal } from './journal'
import { commitLines, omittedCommits } from './runoutcome'
import type { NodeCommits } from './api.gen'
import type { WfEvent } from './types'

const commits: NodeCommits[] = [
  {
    node: 'impl',
    before: 'aaa',
    after: '0123456789abcdef',
    commits: [
      { sha: '0123456789abcdef', subject: 'fix the parser' },
      { sha: 'fedcba9876543210', subject: 'add a failing test' },
    ],
    omitted: 3,
  },
]

describe('commitLines', () => {
  it('lists every commit with a short sha, per node', () => {
    expect(commitLines(commits)).toEqual([
      { node: 'impl', sha: '0123456789abcdef', short: '0123456789ab', subject: 'fix the parser' },
      { node: 'impl', sha: 'fedcba9876543210', short: 'fedcba987654', subject: 'add a failing test' },
    ])
    expect(omittedCommits(commits)).toBe(3)
  })

  it('is empty for a run without commits (the block stays hidden)', () => {
    expect(commitLines(undefined)).toEqual([])
    expect(omittedCommits(undefined)).toBe(0)
  })
})

describe('artifacts_committed in the event journal', () => {
  it('notes the count and the newest commit', () => {
    const events: WfEvent[] = [
      { seq: 1, ts: 1, type: 'artifacts_committed', node: 'impl', before: 'a', after: 'b', commits: commits[0].commits, omitted: 3 },
    ]
    expect(runEventJournal(events)[0].note).toBe('5 commits: 0123456789ab fix the parser')
  })
})

describe('RunOutcomePanel', () => {
  it('renders the commits, and nothing for a run without them', async () => {
    const { render } = await import('svelte/server')
    const Panel = (await import('./RunOutcomePanel.svelte')).default
    const shown = render(Panel, { props: { commits } }).body
    expect(shown).toContain('data-testid="run-commits"')
    expect(shown).toContain('fix the parser')
    expect(shown).toContain('and 3 more')
    const hidden = render(Panel, { props: { commits: undefined } }).body
    expect(hidden).not.toContain('run-commits')
  })
})
