// Connector trust: the connector's own tree digest, or an account's non-secret
// field digest, against the local trust store (design doc section 7/9).
// `invalid` covers a connector that failed to parse at all (a fourth state on
// top of the three the trust store itself distinguishes).
export type ConnectorTrust = 'approved' | 'changed' | 'unapproved' | 'invalid'

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

export interface ConnectorFunction {
  name: string
  description: string
  readOnly: boolean
  deprecated: boolean
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
    default:
      return { label: 'unapproved', tone: 'muted' }
  }
}

export function accountReady(a: ConnectorAccount): boolean {
  return a.missingEnv.length === 0
}
