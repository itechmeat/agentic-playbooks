// Hash-route helpers that are pure enough to unit test outside a component.

// decodeURIComponent throws on malformed percent-encoding (e.g. a lone `%`).
// A bad hash segment must not blow up route parsing, so fall back to the raw
// segment when decoding fails.
export function decodeSegment(s: string): string {
  try {
    return decodeURIComponent(s)
  } catch {
    return s
  }
}

// Connectors are installed machine-wide (`<config_dir>/connectors/`), not per
// project, so their route carries no workspace segment: `#/connector/<name>`.
// The whole remainder is the name, which is why this cannot reuse the
// playbook-style `wsId` parser (with no slash that helper reads the name as
// the workspace and leaves the id empty).
//
// Links minted before the route was fixed used `#/connector//<name>` with an
// empty workspace segment. Those may be open or bookmarked, so a leading empty
// segment is collapsed instead of being treated as part of the name.
export function connectorRouteName(rest: string): string {
  return decodeSegment(rest.replace(/^\/+/, '').replace(/\/+$/, ''))
}

export function connectorHref(name: string): string {
  return `#/connector/${encodeURIComponent(name)}`
}

/** What a route's query asks a page to open. */
export interface RouteFocus {
  node?: string
  edge?: string
  yaml?: boolean
  run?: string
}

/** Splits `#/edit/ws/id?node=x` into its path and the focus it asks for. */
export function splitRoute(hash: string): { path: string; focus: RouteFocus } {
  const q = hash.indexOf('?')
  if (q < 0) return { path: hash, focus: {} }
  const params = new URLSearchParams(hash.slice(q + 1))
  const focus: RouteFocus = {}
  const node = params.get('node')
  const edge = params.get('edge')
  const run = params.get('run')
  if (node) focus.node = node
  if (edge) focus.edge = edge
  if (run) focus.run = run
  if (params.get('yaml') === '1') focus.yaml = true
  return { path: hash.slice(0, q), focus }
}
