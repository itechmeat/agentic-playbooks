import type { SearchFields } from './api.gen'
import { RUN_SEARCH_MAX_TERMS, RUN_SEARCH_MAX_VALUE_CHARS } from './consts.gen'

// The run list's search fields (U45): the params a playbook marks
// `search: true`, as `GET /api/runs?search=1&field=name:value` answers them.
// Matching is the server's (equality, text without case); this module only
// builds the query and formats what came back.

export type SearchTerm = { name: string; value: string }

// The query string of a search listing: `search=1` and one `field` per term,
// at most RUN_SEARCH_MAX_TERMS.
export function searchQuery(terms: SearchTerm[], workspace = ''): string {
  const q = new URLSearchParams()
  if (workspace) q.set('workspace', workspace)
  q.set('search', '1')
  for (const t of terms.slice(0, RUN_SEARCH_MAX_TERMS)) q.append('field', `${t.name}:${t.value}`)
  return `?${q.toString()}`
}

// The field names the listed runs offer, in first-seen order.
export function fieldNames(runs: { search?: SearchFields }[]): string[] {
  const names: string[] = []
  for (const r of runs) {
    if (r.search?.state !== 'indexed') continue
    for (const f of r.search.fields) if (!names.includes(f.name)) names.push(f.name)
  }
  return names
}

// `customer Acme, ticket withheld`: a run's fields, a value shown only when
// the server listed it.
export function fieldsText(search: SearchFields | undefined): string | null {
  if (search?.state !== 'indexed' || search.fields.length === 0) return null
  return search.fields
    .map((f) => `${f.name} ${f.state === 'value' && f.value !== undefined ? f.value : f.state.replace('_', ' ')}`)
    .join(', ')
}

// The footer of a filtered listing, so "no match" never hides "not
// searchable"; null when there is nothing to say.
export function searchFooter(a: { not_indexed: number; withheld: number }, filtered: boolean): string | null {
  const parts: string[] = []
  if (a.not_indexed > 0) {
    parts.push(`${a.not_indexed} ${a.not_indexed === 1 ? 'run has' : 'runs have'} no search fields`)
  }
  if (filtered && a.withheld > 0) {
    parts.push(`${a.withheld} ${a.withheld === 1 ? 'run' : 'runs'} left out: a searched value is withheld`)
  }
  return parts.length ? parts.join('; ') : null
}

// Adds a term, replacing one of the same name; refuses an empty value, a
// value over the character cap and a query over the limit (the server would
// answer 400).
export type AddTermRefusal = 'empty' | 'limit' | 'too_long'

export function addTermChecked(
  terms: SearchTerm[],
  t: SearchTerm,
): { next: SearchTerm[]; refused: AddTermRefusal | null } {
  const value = t.value.trim()
  if (!t.name || !value) return { next: terms, refused: 'empty' }
  if (value.length > RUN_SEARCH_MAX_VALUE_CHARS) return { next: terms, refused: 'too_long' }
  const rest = terms.filter((x) => x.name !== t.name)
  if (rest.length >= RUN_SEARCH_MAX_TERMS) return { next: terms, refused: 'limit' }
  return { next: [...rest, { name: t.name, value }], refused: null }
}

export function addTerm(terms: SearchTerm[], t: SearchTerm): SearchTerm[] {
  return addTermChecked(terms, t).next
}

// Why a term was refused, so the filter row can say so instead of silently
// doing nothing.
export function termRefusalText(refused: AddTermRefusal): string {
  if (refused === 'empty') return 'Enter a value to filter on.'
  if (refused === 'too_long') return `Keep the value to ${RUN_SEARCH_MAX_VALUE_CHARS} characters or fewer.`
  return `At most ${RUN_SEARCH_MAX_TERMS} search terms are kept. Remove one to add another.`
}

// The field to filter on: a name the listed runs offer, never a stale one.
export function chosenName(names: string[], name: string): string {
  return names.includes(name) ? name : (names[0] ?? '')
}

// A 4xx answer is the only thing that can refuse a search; a network or
// 5xx failure is a plain load failure.
export function isSearchRefusal(e: unknown): boolean {
  const status = (e as { status?: number } | undefined)?.status
  return typeof status === 'number' && status >= 400 && status < 500
}

// A refused search's readable reason: the server's detail when it sent one,
// otherwise the message without the request URL.
export function searchErrorText(e: unknown): string {
  const err = e as { detail?: string; message?: string } | undefined
  if (typeof err?.detail === 'string' && err.detail) return err.detail
  const msg = err?.message ?? String(e)
  return msg.replace(/^\/api\/runs[^:]*:\s*/, '')
}
