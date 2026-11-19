import type { FieldView } from './api.gen'

// The review gate's form (people-in-the-loop): the pure half of what the
// panel renders and sends. The server checks every value again when the
// decision is posted; these functions only shape what the browser sends.

/** The values a gate's form starts with: each field's default, or the
 * type's empty value without one. An enum without a default starts at
 * `''`, the panel's empty placeholder option, so nothing is chosen for the
 * person. A bool without a default starts unset (`undefined`), never
 * `false`: it is posted only once the person chose, as on the CLI and MCP. */
export function initialValues(fields: FieldView[]): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const f of fields) {
    if (f.kind === 'bool') {
      out[f.name] = typeof f.default === 'boolean' ? f.default : undefined
      continue
    }
    out[f.name] = f.default ?? (f.kind === 'number' ? null : '')
  }
  return out
}

/** Whether a value of `f` is missing: nothing typed and no default filled. */
function missing(v: unknown): boolean {
  return v === undefined || v === null || (typeof v === 'string' && v.trim() === '')
}

/** A whole decimal number as typed: an optional sign, digits with an
 * optional fraction, an optional exponent. Nothing else reads, so "12abc",
 * "0x10" or "1_000" is no number, never the number it starts with. */
const DECIMAL = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/

/** A number field's value as a number: `null` for no value (nothing or
 * only spaces typed), `NaN` for text that is not a whole finite decimal
 * number. */
function numberOf(v: unknown): number | null {
  if (v === undefined || v === null) return null
  if (typeof v === 'number') return Number.isFinite(v) ? v : NaN
  const t = String(v).trim()
  if (t === '') return null
  if (!DECIMAL.test(t)) return NaN
  const n = Number(t)
  return Number.isFinite(n) ? n : NaN
}

/** The number fields whose typed text is no finite number: the panel names
 * them and posts nothing, never dropping a value the person typed. */
export function invalidNumbers(fields: FieldView[], values: Record<string, unknown>): string[] {
  return fields
    .filter((f) => f.kind === 'number' && Number.isNaN(numberOf(values[f.name]) ?? 0))
    .map((f) => f.name)
}

/** The payload sent with the decision: numbers and bools typed; a value
 * nobody gave is left out, which the server reads as "no value" and keeps
 * the default (or refuses, when the field is required). A field the person
 * edited (`edited` names it) and left blank is sent blank, so the server's
 * one check decides it: a blank is refused for a decision the field is
 * required for, a cleared text stays empty, and a cleared number keeps the
 * default. Whether a required value is missing is never decided here. */
export function toPayload(
  fields: FieldView[],
  values: Record<string, unknown>,
  edited: readonly string[] = [],
): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const f of fields) {
    const v = values[f.name]
    if (f.kind === 'number') {
      const n = numberOf(v)
      if (n !== null && !Number.isNaN(n)) out[f.name] = n
      else if (n === null && edited.includes(f.name)) out[f.name] = ''
    } else if (f.kind === 'bool') {
      if (typeof v === 'boolean') out[f.name] = v
    } else if (f.kind === 'text') {
      if (typeof v === 'string' && (v !== '' || edited.includes(f.name))) out[f.name] = v
    } else if (!missing(v)) {
      out[f.name] = v
    } else if (edited.includes(f.name)) {
      out[f.name] = ''
    }
  }
  return out
}
