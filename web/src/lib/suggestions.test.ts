import { describe, expect, it } from 'vitest'
import {
  ALL_PROJECTS_LABEL,
  GLOBAL_SOURCE_ID,
  kindLabel,
  mergeSuggestionRows,
  removeMessage,
  sortSuggestions,
  suggestionWarnings,
  untilLabel,
  type SuggestionRecord,
  type SuggestionRow,
  type WorkspaceSuggestions,
} from './suggestions'

const row = (over: Partial<SuggestionRow>): SuggestionRow => ({
  pattern: 'p',
  synopsis: 's',
  kind: 'soft',
  scope: 'project',
  declines: 1,
  snoozed_until: '2026-08-05T10:00:00Z',
  workspace_id: 'w',
  project: 'proj',
  ...over,
})

describe('kindLabel', () => {
  // A hard record expires at its hard TTL, so the label must not promise a
  // permanent ban the store does not implement.
  it('names a hard record as a long snooze, never as "never again"', () => {
    expect(kindLabel('hard')).toBe('long snooze')
    expect(kindLabel('hard')).not.toContain('never')
  })
  it('names a soft record as snoozed', () => {
    expect(kindLabel('soft')).toBe('snoozed')
  })
  it('passes an unknown kind through rather than inventing one', () => {
    expect(kindLabel('weird')).toBe('weird')
  })
})

describe('untilLabel', () => {
  const now = Date.parse('2026-07-29T10:00:00Z')
  it('renders a multi-day distance', () => {
    expect(untilLabel('2026-08-05T10:00:00Z', now)).toBe('in 7 days')
  })
  it('renders a single day in the singular', () => {
    expect(untilLabel('2026-07-30T10:00:00Z', now)).toBe('in 1 day')
  })
  it('renders less than a day as today', () => {
    expect(untilLabel('2026-07-29T18:00:00Z', now)).toBe('today')
  })
  it('renders a past date as expired', () => {
    expect(untilLabel('2026-07-01T10:00:00Z', now)).toBe('expired')
  })
  it('renders an unparseable date as a dash', () => {
    expect(untilLabel('not a date', now)).toBe('-')
  })
})

describe('sortSuggestions', () => {
  it('puts hard records first, then sorts by pattern', () => {
    const rows = [
      row({ pattern: 'b-soft', kind: 'soft' }),
      row({ pattern: 'z-hard', kind: 'hard' }),
      row({ pattern: 'a-soft', kind: 'soft' }),
      row({ pattern: 'a-hard', kind: 'hard' }),
    ]
    expect(sortSuggestions(rows).map((r) => r.pattern)).toEqual([
      'a-hard',
      'z-hard',
      'a-soft',
      'b-soft',
    ])
  })
  it('does not mutate its input', () => {
    const rows = [row({ pattern: 'b', kind: 'soft' }), row({ pattern: 'a', kind: 'hard' })]
    sortSuggestions(rows)
    expect(rows[0].pattern).toBe('b')
  })
})

describe('mergeSuggestionRows', () => {
  const globalRecord = (over: Partial<SuggestionRecord> = {}): SuggestionRecord => ({
    pattern: 'g-pattern',
    synopsis: 'a shared decision',
    kind: 'hard',
    scope: 'global',
    declines: 2,
    snoozed_until: '2026-08-05T10:00:00Z',
    ...over,
  })

  const projectRecord = (over: Partial<SuggestionRecord> = {}): SuggestionRecord => ({
    pattern: 'p-pattern',
    synopsis: 'a per-project decision',
    kind: 'soft',
    scope: 'project',
    declines: 1,
    snoozed_until: '2026-08-05T10:00:00Z',
    ...over,
  })

  it('collapses a global record fetched via three workspaces into one row labeled for all projects', () => {
    const perWorkspace: WorkspaceSuggestions[] = [
      { workspace_id: 'w1', project: 'Alpha', records: [globalRecord()] },
      { workspace_id: 'w2', project: 'Beta', records: [globalRecord()] },
      { workspace_id: 'w3', project: 'Gamma', records: [globalRecord()] },
    ]
    const merged = mergeSuggestionRows(perWorkspace)
    expect(merged).toHaveLength(1)
    expect(merged[0].pattern).toBe('g-pattern')
    expect(merged[0].project).toBe(ALL_PROJECTS_LABEL)
  })

  it('does not collapse project-scope rows with identical patterns across different workspaces', () => {
    const perWorkspace: WorkspaceSuggestions[] = [
      { workspace_id: 'w1', project: 'Alpha', records: [projectRecord()] },
      { workspace_id: 'w2', project: 'Beta', records: [projectRecord()] },
    ]
    const merged = mergeSuggestionRows(perWorkspace)
    expect(merged).toHaveLength(2)
    expect(merged.map((r) => r.workspace_id)).toEqual(['w1', 'w2'])
    expect(merged.map((r) => r.project)).toEqual(['Alpha', 'Beta'])
  })

  // The workspace-less GET is the only source of global records on a machine
  // with zero registered projects. When both sources are present the global
  // one is listed first, so the surviving row is addressed without a
  // workspace, which is what a global DELETE needs.
  it('collapses the workspace-less global source together with the per-workspace copies', () => {
    const perWorkspace: WorkspaceSuggestions[] = [
      { workspace_id: GLOBAL_SOURCE_ID, project: '', records: [globalRecord()] },
      { workspace_id: 'w1', project: 'Alpha', records: [globalRecord()] },
      { workspace_id: 'w2', project: 'Beta', records: [globalRecord(), projectRecord()] },
    ]
    const merged = mergeSuggestionRows(perWorkspace)
    expect(merged).toHaveLength(2)
    const global = merged.find((r) => r.scope === 'global')
    expect(global?.workspace_id).toBe(GLOBAL_SOURCE_ID)
    expect(global?.project).toBe(ALL_PROJECTS_LABEL)
    expect(merged.find((r) => r.scope === 'project')?.project).toBe('Beta')
  })

  it('keeps scope global on the merged row, so the delete payload still carries scope global', () => {
    const perWorkspace: WorkspaceSuggestions[] = [
      { workspace_id: 'w1', project: 'Alpha', records: [globalRecord()] },
      { workspace_id: 'w2', project: 'Beta', records: [globalRecord()] },
    ]
    const merged = mergeSuggestionRows(perWorkspace)
    expect(merged).toHaveLength(1)
    expect(merged[0].scope).toBe('global')
    expect(merged[0].workspace_id).toBe('w1')
  })
})

// A failed fetch used to collapse into an empty array, and the server's own
// diagnostics were dropped on the floor, so a corrupt store looked exactly
// like "no decisions recorded".
describe('suggestionWarnings', () => {
  it('reports a workspace whose fetch failed', () => {
    const warnings = suggestionWarnings([
      { workspace_id: 'w1', project: 'Alpha', records: [], error: 'HTTP 410' },
      { workspace_id: 'w2', project: 'Beta', records: [] },
    ])
    expect(warnings).toHaveLength(1)
    expect(warnings[0]).toContain('Alpha')
    expect(warnings[0]).toContain('HTTP 410')
  })

  it('passes the server diagnostics through, attributed to their project', () => {
    const warnings = suggestionWarnings([
      {
        workspace_id: 'w1',
        project: 'Alpha',
        records: [],
        diagnostics: ['malformed suggestion store `/x/.apb/suggestions.json`'],
      },
    ])
    expect(warnings).toEqual(['Alpha: malformed suggestion store `/x/.apb/suggestions.json`'])
  })

  it('labels a diagnostic from the workspace-less global source without a project name', () => {
    const warnings = suggestionWarnings([
      { workspace_id: GLOBAL_SOURCE_ID, project: '', records: [], diagnostics: ['no global config dir'] },
    ])
    expect(warnings).toEqual(['global: no global config dir'])
  })

  it('says nothing when every workspace loaded cleanly', () => {
    expect(
      suggestionWarnings([{ workspace_id: 'w1', project: 'Alpha', records: [], diagnostics: [] }]),
    ).toEqual([])
  })
})

// The dashboard used to show "can be suggested again" next to a card that
// reappeared on the very next load, because the other scope still suppressed
// the pattern.
describe('removeMessage', () => {
  it('promises a re-offer only when nothing suppresses the pattern any more', () => {
    expect(removeMessage('p', { removed: true, still_suppressed: false })).toEqual({
      title: '"p" can be suggested again',
    })
  })

  it('names the scope that still silences the suggestion', () => {
    const msg = removeMessage('p', {
      removed: true,
      still_suppressed: true,
      still_suppressed_by: 'global',
    })
    expect(msg.title).not.toContain('can be suggested again')
    expect(msg.title).toContain('removed')
    expect(msg.description).toContain('global')
  })

  it('treats a response without the field as "nothing else suppresses it"', () => {
    expect(removeMessage('p', { removed: true }).title).toBe('"p" can be suggested again')
  })
})
