// A node's connector binding in YAML - a string shorthand (everything
// default) or an object, the same two-form pattern as ProfileRef/SkillRef
// (mirrors profileref.ts). Separate module so the round trip is
// unit-testable against the backend's exact serialization (review: a
// fully-default binding must collapse back to the bare string, and the
// `read_only` shorthand must never expand into an explicit list).
//
// Mirrors crates/apb-core/src/schema.rs `ConnectorBinding`/`FunctionsAllow`
// exactly: `functions` is a required field defaulting to `'all'` (backend
// `FunctionsAllow::All`, an absent YAML field), never actually optional in
// the resolved value even though the TS field is marked `?` for construction
// convenience. `accounts` mirrors `Option<Vec<String>>` - undefined means
// "every account". `maxCalls` mirrors `Option<u32>` - undefined means no
// budget; 0 is invalid per validator V26 and is never emitted.
export type FunctionsAllow = 'all' | 'read_only' | string[]

export interface ConnectorBinding {
  name: string
  accounts?: string[]
  functions?: FunctionsAllow
  maxCalls?: number
}

function normalizeFunctions(f: FunctionsAllow | undefined): FunctionsAllow {
  return f ?? 'all'
}

/** YAML value (string shorthand or object) -> in-memory binding. */
export function parseBinding(yaml: unknown): ConnectorBinding {
  if (typeof yaml === 'string') {
    return { name: yaml, functions: 'all' }
  }
  if (yaml && typeof yaml === 'object' && !Array.isArray(yaml)) {
    const o = yaml as Record<string, unknown>
    const name = typeof o.name === 'string' ? o.name : ''
    const binding: ConnectorBinding = { name, functions: 'all' }
    if (Array.isArray(o.accounts) && o.accounts.every((a) => typeof a === 'string')) {
      binding.accounts = o.accounts as string[]
    }
    if (o.functions === 'read_only') {
      binding.functions = 'read_only'
    } else if (Array.isArray(o.functions) && o.functions.every((x) => typeof x === 'string')) {
      binding.functions = o.functions as string[]
    }
    if (typeof o.max_calls === 'number' && Number.isFinite(o.max_calls)) {
      binding.maxCalls = o.max_calls
    }
    return binding
  }
  // Malformed/unsupported shape (number, array, null, ...): no valid name to
  // recover, callers filter empty-name bindings before writing them back.
  return { name: '', functions: 'all' }
}

/**
 * In-memory binding -> value to write into YAML: bare string when accounts,
 * functions, and max_calls are all default (matches the backend's Serialize
 * impl for ConnectorBinding exactly, so diffs stay stable), otherwise an
 * object with only the non-default fields set. `max_calls <= 0` or
 * non-integer is treated as absent (defensive: the UI already blocks 0/neg
 * client-side via the input's min=1, this is the last line of defense
 * against writing an invalid binding, V26).
 */
export function serializeBinding(b: ConnectorBinding): unknown {
  const functions = normalizeFunctions(b.functions)
  const hasAccounts = b.accounts !== undefined
  const hasFunctions = functions !== 'all'
  const hasMaxCalls =
    typeof b.maxCalls === 'number' && Number.isInteger(b.maxCalls) && b.maxCalls >= 1

  if (!hasAccounts && !hasFunctions && !hasMaxCalls) {
    return b.name
  }

  const obj: Record<string, unknown> = { name: b.name }
  if (hasAccounts) obj.accounts = b.accounts
  if (hasFunctions) obj.functions = functions
  if (hasMaxCalls) obj.max_calls = b.maxCalls
  return obj
}

/**
 * Toggles `entry` in/out of an allowlist that uses the "absent = every
 * entry" convention (both `accounts` and an explicit `functions` list share
 * this shape). `list` undefined means every entry in `all` is currently
 * checked. Returns the new list, collapsing back to `undefined` (as a set,
 * order-independent) when the result covers every entry in `all` again -
 * this is what lets an "uncheck one, recheck it" cycle round-trip back to
 * the absent allowlist form instead of settling on an explicit full list.
 */
export function toggleListEntry(
  list: string[] | undefined,
  entry: string,
  all: string[],
): string[] | undefined {
  const current = list ?? all
  const next = current.includes(entry) ? current.filter((x) => x !== entry) : [...current, entry]
  if (next.length === all.length && all.every((a) => next.includes(a))) {
    return undefined
  }
  return next
}
