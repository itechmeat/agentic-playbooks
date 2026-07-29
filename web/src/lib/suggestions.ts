// The suggestion-decision records the dashboard shows on the playbooks page:
// what the agent recorded when the user declined a save-as-playbook offer, and
// for how long that silences the offer. Pure formatting and ordering here; the
// fetching lives in `api/core.ts` and the rendering in
// `components/SuggestionsSection.svelte`.

export interface SuggestionRecord {
  pattern: string
  synopsis: string
  kind: 'soft' | 'hard'
  scope: 'project' | 'global'
  declines: number
  // RFC-3339 UTC, as the server renders it.
  snoozed_until: string
}

// A record plus the project it was read for: the playbooks page aggregates
// every registered workspace, so a row has to say which one it belongs to.
export interface SuggestionRow extends SuggestionRecord {
  workspace_id: string
  project: string
}

const DAY_MS = 24 * 60 * 60 * 1000

// One source's raw fetch result: the records the server returned, the project
// they belong to, whatever the server had to say about reading them, and the
// reason the fetch failed when it did. Input shape for `mergeSuggestionRows`
// and `suggestionWarnings`.
//
// `diagnostics` and `error` are what makes a broken store visible: dropping
// them turned every failure into an empty list, so a corrupt file looked
// exactly like "nothing is silenced here".
export interface WorkspaceSuggestions {
  workspace_id: string
  project: string
  records: SuggestionRecord[]
  diagnostics?: string[]
  error?: string
}

// The label a global-scope row carries in place of a single project name:
// the record isn't owned by any one project, it applies everywhere.
export const ALL_PROJECTS_LABEL = 'all projects'

// The workspace id of the workspace-LESS source: `GET /api/suggestions` with
// no `workspace` param serves the global store on its own, which is the only
// way to reach machine-wide records on a machine with zero registered
// projects. An empty id also means a row that came from it is addressed
// without a workspace, exactly as a global DELETE needs.
export const GLOBAL_SOURCE_ID = ''

// Turns each workspace's fetch result into rows, collapsing global-scope
// duplicates. A global record lives in one shared store, but `GET
// /api/suggestions` merges it into every workspace's view, so naively mapping
// each workspace's records to rows would render the same global record once
// per registered project. Keep exactly one row per distinct global pattern
// (whichever workspace's copy is seen first, since they are identical) and
// label it for all projects rather than the one workspace it happened to come
// from. Project-scope records keep their own workspace's attribution
// untouched, even when two different projects share a pattern - those are
// genuinely separate records and must not collapse.
export function mergeSuggestionRows(perWorkspace: WorkspaceSuggestions[]): SuggestionRow[] {
  const rows: SuggestionRow[] = []
  const seenGlobalPatterns = new Set<string>()
  for (const { workspace_id, project, records } of perWorkspace) {
    for (const r of records) {
      if (r.scope === 'global') {
        if (seenGlobalPatterns.has(r.pattern)) continue
        seenGlobalPatterns.add(r.pattern)
        rows.push({ ...r, workspace_id, project: ALL_PROJECTS_LABEL })
      } else {
        rows.push({ ...r, workspace_id, project })
      }
    }
  }
  return rows
}

// The compact warning lines the section shows above the cards: one per source
// that could not be read at all, one per diagnostic the server reported for a
// source it could read. Attribution matters more than brevity here, so each
// line names the project it came from (or "global" for the workspace-less
// source, which belongs to no project).
export function suggestionWarnings(perWorkspace: WorkspaceSuggestions[]): string[] {
  const lines: string[] = []
  for (const { workspace_id, project, diagnostics, error } of perWorkspace) {
    const who = project || (workspace_id === GLOBAL_SOURCE_ID ? 'global' : workspace_id)
    if (error) lines.push(`${who}: could not be read (${error})`)
    for (const d of diagnostics ?? []) lines.push(`${who}: ${d}`)
  }
  return lines
}

// What `DELETE /api/suggestions/{pattern}` answers. `still_suppressed` is
// additive: an older server omits it, which reads as "nothing else silences
// this pattern", the behavior that surface always assumed.
export interface RemoveResult {
  removed: boolean
  still_suppressed?: boolean
  still_suppressed_by?: string | null
}

// The toast for one removal. Removing a record in one scope does not
// necessarily bring the offer back: the same pattern can live in the other
// store too, and the card then reappears on the very next load. The message
// promises a re-offer only when the server says nothing suppresses the
// pattern any more. `removed` is checked first: a call that found no record
// must never be read as a re-enable or as "still silenced" (both would claim
// something the server never did), so it gets the same "not found" wording
// the CLI uses.
export function removeMessage(
  pattern: string,
  result: RemoveResult,
): { title: string; description?: string } {
  if (!result.removed) return { title: `no "${pattern}" record found` }
  if (!result.still_suppressed) return { title: `"${pattern}" can be suggested again` }
  const by = result.still_suppressed_by ?? 'another'
  return {
    title: `"${pattern}" removed, but still silenced`,
    description: `The ${by} record still silences this suggestion. Remove that one too to bring the offer back.`,
  }
}

// Human label for the kind. A hard record is deliberately NOT labelled "never
// again": it carries a hard TTL (90 days by default), so it silences the
// suggestion for a long fixed window and then expires. The label says how long
// the silence is, not that it is forever, and the until-badge next to it names
// the actual date. An unknown value is passed through verbatim rather than
// mapped to one of the two known labels, so a future kind is visible instead of
// silently mislabelled.
export function kindLabel(kind: string): string {
  if (kind === 'hard') return 'long snooze'
  if (kind === 'soft') return 'snoozed'
  return kind
}

// How much silence is left. Whole days, because the backoff schedule is in
// days; anything under a day reads as "today" and a past date as "expired"
// (a record can be listed and then expire while the page is open).
export function untilLabel(snoozedUntilIso: string, nowMs: number): string {
  const until = Date.parse(snoozedUntilIso)
  if (Number.isNaN(until)) return '-'
  const delta = until - nowMs
  if (delta <= 0) return 'expired'
  const days = Math.floor(delta / DAY_MS)
  if (days === 0) return 'today'
  return days === 1 ? 'in 1 day' : `in ${days} days`
}

// Hard records first (the longest silences, the ones the user is most likely
// looking for), then by pattern. Returns a new array.
export function sortSuggestions(rows: SuggestionRow[]): SuggestionRow[] {
  return [...rows].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'hard' ? -1 : 1
    return a.pattern.localeCompare(b.pattern)
  })
}
