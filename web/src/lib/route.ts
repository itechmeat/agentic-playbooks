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
