// Connector trust: the connector's own tree digest, or an account's non-secret
// field digest, against the local trust store (design doc section 7/9).
// `invalid` covers a connector that failed to parse at all (a fourth state on
// top of the three the trust store itself distinguishes). `not_installed` is
// the detail endpoint's answer for an embedded connector with no bytes on
// disk: there is nothing to have decided trust about yet, which is a state of
// its own and not a trust problem.
export type ConnectorTrust =
  | 'approved'
  | 'changed'
  | 'unapproved'
  | 'invalid'
  | 'not_installed'

export interface ConnectorCard {
  name: string
  version: string
  displayName: string
  summary: string
  tags: string[]
  trust: ConnectorTrust
  accountsTotal: number
  accountsReady: number
}

export interface ConnectorAccount {
  name: string
  default: boolean
  // Non-secret fields only, verbatim (e.g. a raw `{{env.VAR}}` reference) -
  // never a resolved secret value.
  fields: Record<string, string>
  // Names of env vars this account needs that do not currently resolve.
  // Never a value.
  missingEnv: string[]
  trust: ConnectorTrust
}

// A minimal JSON Schema subset: just enough to describe one function's
// `args_schema` (design doc section 4.4) for the playground's generated
// form (spec section 7). Not a general JSON Schema type.
export interface JsonSchemaProperty {
  type?: string
  enum?: (string | number)[]
  description?: string
  default?: unknown
  oneOf?: unknown[]
  anyOf?: unknown[]
}
export interface JsonSchema {
  type?: string
  properties?: Record<string, JsonSchemaProperty>
  required?: string[]
  oneOf?: unknown[]
  anyOf?: unknown[]
}

export interface ConnectorFunction {
  name: string
  description: string
  readOnly: boolean
  // The manifest's deprecation reason, verbatim, or null when the function is
  // current. The backend field is `Option<String>` (a human-readable reason),
  // so a boolean here would throw the text away.
  deprecated: string | null
  argsSchema: JsonSchema | null
}

export interface ConnectorMeta {
  display_name?: string
  summary?: string
  tags?: string[]
  publisher?: string
  homepage?: string
  icon?: string
}

export interface ConnectorDetail {
  name: string
  version: string
  // False for an embedded official connector that is offered but has not been
  // connected yet. Everything else on this object is manifest-derived and is
  // populated either way.
  installed: boolean
  trust: ConnectorTrust
  meta: ConnectorMeta
  bodyMd: string
  functions: ConnectorFunction[]
  accounts: ConnectorAccount[]
}

export function trustBadge(t: ConnectorTrust): {
  label: string
  tone: 'ok' | 'warn' | 'muted' | 'danger'
} {
  switch (t) {
    case 'approved':
      return { label: 'approved', tone: 'ok' }
    case 'changed':
      return { label: 'changed', tone: 'warn' }
    case 'invalid':
      return { label: 'invalid', tone: 'danger' }
    case 'not_installed':
      return { label: 'not connected', tone: 'muted' }
    default:
      return { label: 'unapproved', tone: 'muted' }
  }
}

export function accountReady(a: ConnectorAccount): boolean {
  return a.missingEnv.length === 0
}

// The deprecation reason to show next to a function name, or null when the
// function is not deprecated. A manifest that sets `deprecated: ""` (or a
// string of spaces) has said nothing, so it must not read as deprecated;
// `null` renders no marker either.
export function deprecationReason(f: ConnectorFunction): string | null {
  const reason = f.deprecated?.trim()
  return reason ? reason : null
}

export function isDeprecated(f: ConnectorFunction): boolean {
  return deprecationReason(f) !== null
}

// Section visibility on the connector detail page.
//
// The rule is not "hide what is empty". An empty card is right whenever the
// section can still be filled from the state the user is in, because then its
// empty state is a call to action. A card is hidden only when it cannot become
// meaningful at all from here.
//
// So About and Functions are hidden when empty: both are manifest-derived and
// the user cannot add to them, making "nothing here" the permanent answer.
// Accounts is the opposite: on an installed connector it is where accounts get
// configured, so it renders empty too.
export function showAbout(d: ConnectorDetail): boolean {
  return d.bodyMd.trim().length > 0
}

export function showFunctions(d: ConnectorDetail): boolean {
  return d.functions.length > 0
}

// Installed: always, since this card is where accounts are configured and its
// empty state is the invitation to do so.
// Not installed: only when accounts already exist. Account configuration lives
// outside the installed files, so a connector that is not connected can
// legitimately already have accounts on disk, and that is worth showing: it
// says the configuration will be picked up on connect.
export function showAccounts(d: ConnectorDetail): boolean {
  return d.installed || d.accounts.length > 0
}

// The playground both needs the installed files (it really invokes the
// connector) and needs something to invoke.
export function showPlayground(d: ConnectorDetail): boolean {
  return d.installed && d.functions.length > 0
}

// The header card's meta row holds tags, publisher, and homepage. With none of
// them it is an empty band of padding.
export function showHeaderMeta(d: ConnectorDetail): boolean {
  return (d.meta.tags?.length ?? 0) > 0 || !!d.meta.publisher || !!d.meta.homepage
}
