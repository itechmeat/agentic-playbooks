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
  deprecated: boolean
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
