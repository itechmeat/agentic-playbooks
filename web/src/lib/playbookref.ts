// A node's playbook ref in YAML - a string (scope auto) OR an object
// { id, scope } (apb_core::schema::QualifiedPlaybookRef, spec C). In the UI
// field we encode it as `scope/id` for an explicit scope, otherwise a bare
// `id` (scope auto) - mirrors profileref.ts exactly, keyed by `id` instead of
// `name`. Separate module so the round trip is unit-testable (review R1-I7:
// an explicit global ref must not degrade to a bare string -> scope auto on
// save, and the object form must not render as "[object Object]").

// The ref's scope is only project/global (auto is encoded as a bare string
// with no prefix).
export type PlaybookRefScope = 'project' | 'global'
export type PlaybookRef = string | { id: string; scope: PlaybookRefScope }

// Mirrors apb_core::registry::is_safe_segment: non-empty, no path separators,
// no `..`. Playbook ids are not restricted to the profile-name character
// class server-side, so this module doesn't invent a tighter one either.
export function isValidPlaybookId(s: string): boolean {
  return s.length > 0 && !s.includes('/') && !s.includes('\\') && !s.includes('..')
}

/** Ref -> UI field string. An invalid/unsupported scope yields no prefix. */
export function playbookRefToField(v: unknown): string {
  if (typeof v === 'string') return v
  if (v && typeof v === 'object') {
    const o = v as { id?: unknown; scope?: unknown }
    const id = typeof o.id === 'string' ? o.id : ''
    const scope = typeof o.scope === 'string' ? o.scope : ''
    if (!id) return ''
    return scope === 'project' || scope === 'global' ? `${scope}/${id}` : id
  }
  return ''
}

/**
 * UI field string -> value to write into YAML: undefined | bare string |
 * { id, scope }. Malformed values (unsupported scope, an id with a nested
 * `/`, e.g. `global/foo/bar`) are rejected by returning undefined - no broken
 * ref ends up in YAML.
 */
export function fieldToPlaybookRef(raw: string): PlaybookRef | undefined {
  const trimmed = raw.trim()
  if (trimmed === '') return undefined
  const slash = trimmed.indexOf('/')
  if (slash > 0) {
    const scope = trimmed.slice(0, slash)
    const id = trimmed.slice(slash + 1)
    if ((scope === 'project' || scope === 'global') && isValidPlaybookId(id)) {
      return { id, scope }
    }
    return undefined // unsupported scope or invalid/nested id
  }
  return isValidPlaybookId(trimmed) ? trimmed : undefined
}
