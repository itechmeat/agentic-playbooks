// A node's profile ref in YAML - a string (scope auto) OR an object
// { name, scope }. In the UI field we encode it as `scope/name` for an
// explicit scope, otherwise a bare `name` (scope auto). Separate module so the
// round trip is unit-testable (review: an explicit global ref must not
// degrade to a bare string -> scope auto, and same-named project/global must
// not collide by value).

// The ref's scope is only project/global (auto is encoded as a bare string
// with no prefix). The profile name uses the same format as the backend
// (validate_profile_name): [a-z0-9][a-z0-9-]*, max 64 characters.
export type ProfileScope = 'project' | 'global'
export type ProfileRef = string | { name: string; scope: ProfileScope }

const NAME_RE = /^[a-z0-9][a-z0-9-]*$/

/** Whether a single profile-name segment is valid (matches validate_profile_name). */
export function isValidProfileName(s: string): boolean {
  return s.length > 0 && s.length <= 64 && NAME_RE.test(s)
}

/** Ref -> UI field string. An invalid/unsupported scope yields no prefix. */
export function profileToField(v: unknown): string {
  if (typeof v === 'string') return v
  if (v && typeof v === 'object') {
    const o = v as { name?: unknown; scope?: unknown }
    const name = typeof o.name === 'string' ? o.name : ''
    const scope = typeof o.scope === 'string' ? o.scope : ''
    if (!name) return ''
    return scope === 'project' || scope === 'global' ? `${scope}/${name}` : name
  }
  return ''
}

/**
 * UI field string -> value to write into YAML: undefined | bare string |
 * { name, scope }. Malformed values (unsupported scope, a name with invalid
 * characters or a nested `/`, e.g. `global/foo/bar`) are rejected by
 * returning undefined - no broken ref ends up in YAML.
 */
export function fieldToProfile(raw: string): ProfileRef | undefined {
  const trimmed = raw.trim()
  if (trimmed === '') return undefined
  const slash = trimmed.indexOf('/')
  if (slash > 0) {
    const scope = trimmed.slice(0, slash)
    const name = trimmed.slice(slash + 1)
    if ((scope === 'project' || scope === 'global') && isValidProfileName(name)) {
      return { name, scope }
    }
    return undefined // unsupported scope or invalid/nested name
  }
  return isValidProfileName(trimmed) ? trimmed : undefined
}

/**
 * Whether a node without its own profile runs on `defaults.profile`: an agent
 * task, and a finish node that composes an answer (it has a `prompt`), as the
 * engine's `effective_profile_ref` resolves it.
 */
export function usesDefaultProfile(n: { type: string; prompt?: unknown }): boolean {
  return n.type === 'agent_task' || (n.type === 'finish' && typeof n.prompt === 'string' && n.prompt.trim() !== '')
}

/** The key of a profile in a `profileModelMap`: the bare name for scope auto, `scope:name` for an explicit scope. */
export function profileModelKey(name: string, scope?: ProfileScope): string {
  return scope ? `${scope}:${name}` : name
}
