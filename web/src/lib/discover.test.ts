import { describe, expect, it } from 'vitest'
import {
  bytesText,
  canAct,
  defaultRoots,
  readLines,
  refusalText,
  projectsText,
  rejectedText,
  savedText,
  sourceText,
  statusText,
  stepsText,
  supportText,
} from './discover'
import { ApiError } from './api/http'
import type { Suggestion } from './api.gen'

const suggestion = (extra: Partial<Suggestion> = {}): Suggestion => ({
  slug: 'seq-0123456789',
  steps: ['cargo:test', 'git:commit', 'git:push'],
  support: { sessions: 3, days: 2, projects: [{ workspace_id: 'ws-1', name: 'app' }] },
  score: 12,
  title: 'Repeated steps: cargo test, git commit, git push',
  synopsis: {},
  yaml: 'schema: 2',
  effects: ['network', 'external'],
  examples: [],
  sources: [],
  status: { state: 'new' },
  ...extra,
})

describe('discover formatting', () => {
  it('reads steps, support and sources the way the CLI does', () => {
    expect(stepsText(['cargo:test', 'git:push'])).toBe('cargo:test > git:push')
    expect(supportText(suggestion())).toBe('3 sessions on 2 days in app')
    expect(supportText(suggestion({ support: { sessions: 1, days: 1, projects: [] } }))).toBe('1 session on 1 day')
    expect(sourceText({ root: 'claude_code', file: 'app/s1.jsonl', workspace_id: 'ws-1', lines: [5, 10] })).toBe(
      'claude_code: app/s1.jsonl, lines 5-10',
    )
    expect(sourceText({ root: 'codex', file: 'app/rollout-x.jsonl', workspace_id: '', lines: [4, 4] })).toBe(
      'codex: app/rollout-x.jsonl, line 4',
    )
    expect(
      projectsText([
        { workspace_id: 'ws-1', name: 'app' },
        { workspace_id: 'ws-2', name: 'app' },
      ]),
    ).toBe('app, app')
  })

  it('names a status and offers actions only on a new suggestion of a signed result', () => {
    expect(statusText({ state: 'new' })).toBe('new')
    expect(statusText({ state: 'saved', playbook: 'seq-1', version: '0.1.0', scope: 'project' })).toBe(
      'saved as seq-1 (project)',
    )
    expect(statusText({ state: 'dismissed' })).toBe('declined')
    expect(canAct({ signed: true }, suggestion())).toBe(true)
    expect(canAct({ signed: false }, suggestion())).toBe(false)
    expect(canAct({ signed: true }, suggestion({ status: { state: 'dismissed' } }))).toBe(false)
  })

  it('summarises what was read and picks the available roots', () => {
    const lines = readLines({
      roots: [
        {
          name: 'claude_code',
          agent: 'claude_code',
          files_seen: 14,
          files_read: 12,
          bytes_read: 3 * 1024 * 1024,
          skipped: { too_old: 2, symlink: 0 },
          cut_files: 1,
        },
      ],
      sessions: 9,
    })
    expect(lines).toEqual(['claude_code (claude_code): 12 of 14 files, 3.0 MB, 1 read in part, skipped: too old 2'])
    expect(bytesText(512)).toBe('512 B')
    expect(bytesText(2048)).toBe('2.0 KB')
    expect(
      defaultRoots([
        { name: 'claude_code', agent: 'claude_code', available: true },
        { name: 'codex', agent: 'codex', available: false },
      ]),
    ).toEqual(['claude_code'])
  })

  it('words a refusal for a person', () => {
    expect(refusalText(new ApiError('/api/discover: discover_local_only', 403))).toBe(
      'Session history is read only from a browser on this machine.',
    )
    expect(refusalText(new ApiError('/api/discover: busy: another discovery is running', 409))).toBe(
      'Another discovery is running. Try again when it is done.',
    )
    expect(refusalText(new ApiError('/api/discover: unknown history root `x`', 422))).toBe('unknown history root `x`')
    // A browser reports the absolute URL of the request.
    expect(
      refusalText(new ApiError('http://127.0.0.1:7357/api/discover: busy: another discovery is running', 409)),
    ).toBe('Another discovery is running. Try again when it is done.')
    expect(refusalText(new ApiError('http://localhost:7357/api/discover/disc-1/seq-1/save: already saved', 409))).toBe(
      'already saved',
    )
    expect(refusalText('offline')).toBe('offline')
    expect(savedText({ id: 'ship-flow' })).toBe('Saved as draft playbook ship-flow')
    expect(savedText({})).toBe('Saved as a draft playbook')
    expect(rejectedText('secret_like_value')).toContain('secret')
    expect(rejectedText('possible_duplicate')).toContain('same trigger')
  })
})
