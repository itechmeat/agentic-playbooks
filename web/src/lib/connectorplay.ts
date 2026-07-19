// Form generation and result rendering for the connector playground panel
// (spec 2026-07-19-official-connectors-design section 7). Pure functions
// only, no DOM - the panel component (ConnectorPlaygroundPanel.svelte)
// stays thin and calls into this module, mirroring connectorstats.ts.

import type { JsonSchema, JsonSchemaProperty } from './connectors'

export type PlayFieldKind = 'string' | 'number' | 'boolean' | 'enum' | 'unsupported'

export interface PlayField {
  name: string
  kind: PlayFieldKind
  required: boolean
  description?: string
  enumValues?: (string | number)[]
}

function fieldKind(prop: JsonSchemaProperty): PlayFieldKind {
  if (Array.isArray(prop.enum) && prop.enum.length > 0) return 'enum'
  switch (prop.type) {
    case 'string':
      return 'string'
    case 'number':
    case 'integer':
      return 'number'
    case 'boolean':
      return 'boolean'
    default:
      return 'unsupported'
  }
}

// Whether a schema is simple enough for the generated form: object type,
// with properties, all of which are simple leaves (string/number/boolean/
// enum). Anything else falls back to the raw JSON textarea; the form
// generator does not attempt partial coverage of a complex schema.
export function isSimpleObjectSchema(schema: JsonSchema | null | undefined): boolean {
  if (!schema || schema.type !== 'object' || !schema.properties) return false
  return Object.values(schema.properties).every((p) => fieldKind(p) !== 'unsupported')
}

// Builds the ordered field list for the generated form from an args_schema.
export function buildPlayFields(schema: JsonSchema | null | undefined): PlayField[] {
  if (!schema?.properties) return []
  const required = new Set(schema.required ?? [])
  return Object.entries(schema.properties).map(([name, prop]) => ({
    name,
    kind: fieldKind(prop),
    required: required.has(name),
    description: prop.description,
    enumValues: prop.enum,
  }))
}

// Coerces the raw values the form widgets produce into the JSON-typed args
// object a call expects. An empty string on a non-required field is
// omitted; an unparsable number is omitted rather than sent as NaN.
export function coerceFormValues(
  fields: PlayField[],
  values: Record<string, string | boolean>,
): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const field of fields) {
    const raw = values[field.name]
    if (field.kind === 'boolean') {
      out[field.name] = raw === true
      continue
    }
    if (raw === undefined || raw === '') continue
    if (field.kind === 'number') {
      const n = Number(raw)
      if (!Number.isNaN(n)) out[field.name] = n
      continue
    }
    out[field.name] = raw
  }
  return out
}

// Parses the raw-JSON textarea fallback. Empty text is an empty args
// object. Throws a descriptive error on invalid JSON or a non-object top
// level - connector args are always an object.
export function parseRawArgs(text: string): Record<string, unknown> {
  const trimmed = text.trim()
  if (trimmed === '') return {}
  let parsed: unknown
  try {
    parsed = JSON.parse(trimmed)
  } catch (e) {
    throw new Error(`invalid JSON: ${String(e instanceof Error ? e.message : e)}`)
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error('args must be a JSON object')
  }
  return parsed as Record<string, unknown>
}

// --- result rendering ------------------------------------------------------

export interface PlayCallError {
  code: string
  message: string
  http_status?: number
  retry_after_sec?: number
}

// The executor's structured outcome, returned verbatim by POST
// /api/connectors/{name}/call (like HealthcheckResult, no camelCase
// mapping - this is a passthrough JSON blob, not a fetch-boundary DTO).
export interface PlayCallResult {
  ok: boolean
  status?: number
  body?: unknown
  truncated?: boolean
  link?: string | null
  picked?: boolean
  dry_run?: boolean
  method?: string
  url?: string
  error?: PlayCallError
}

// A one-line status summary for the result panel header.
export function resultSummary(r: PlayCallResult): string {
  if (r.dry_run) return `dry run: ${r.method ?? ''} ${r.url ?? ''}`.trim()
  if (r.ok) return `${r.status ?? ''} ok`.trim()
  const status = r.error?.http_status ? ` (HTTP ${r.error.http_status})` : ''
  return `${r.error?.code ?? 'error'}${status}`
}

// Pretty-prints the body (success or dry run) or the error object
// (failure) for the result panel, 2-space indented JSON.
export function formatResultBody(r: PlayCallResult): string {
  if (!r.ok) return JSON.stringify(r.error ?? {}, null, 2)
  return JSON.stringify(r.body ?? null, null, 2)
}
