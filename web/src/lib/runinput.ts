// The run input of the Run dialog: one field per playbook param, prefilled
// from what this project remembers (`GET .../memory`), and the run body it
// sends. Pure, so the rules are tested without the dialog:
// - a compatible remembered value is prefilled, else the param's default;
// - a secret param is never prefilled (a password input, asked every time);
// - a remembered value that no longer fits is shown with the reason, never
//   used: the field shows the default the start will use instead;
// - the body carries what the person chose, nothing more: a value equal to
//   the default goes out only when it replaces a remembered one, and a
//   cleared remembered value goes out as the default, so the start never
//   falls back to the value the dialog no longer shows (and `remember`
//   never stores a default nobody chose). When memory could not be read,
//   the start must not pick up values the dialog never showed, so it sends
//   every value it shows (the default for an empty field).
import type { Incompat, Recall } from './api.gen'
import type { RunInput } from './api'
import type { PlaybookParam } from './types'

export interface ParamField {
  name: string
  label: string
  type: string
  options: string[]
  secret: boolean
  value: string
  /** The param's default ('' when it has none). */
  fallback: string
  /** Whether the param declares a default (`default: ""` included). */
  hasDefault: boolean
  /** The compatible remembered value a start would use, if any. */
  remembered: string | null
  /** Whether memory was read: without it, what a start would recall is
   * unknown, so every shown value is sent. */
  memoryKnown: boolean
  /** Why the remembered value is not used, or a secret's hint. */
  note: string | null
  /** A secret, or no default at all: the start must pass a value. */
  required: boolean
}

export function incompatText(reason: Incompat): string {
  switch (reason) {
    case 'removed':
      return 'the param is gone'
    case 'type_changed':
      return 'the remembered value no longer has the right type'
    case 'option_gone':
      return 'the remembered value is no longer one of the options'
    case 'now_secret':
      return 'the param is a secret now; enter it every time'
  }
}

function text(v: unknown): string {
  if (v === null || v === undefined) return ''
  return typeof v === 'string' ? v : String(v)
}

export function paramFields(params: PlaybookParam[], recall: Recall | null): ParamField[] {
  return params.map((p) => {
    const recalled = recall?.values.find((v) => v.name === p.name)
    const fallback = text(p.default)
    const hasDefault = p.default !== undefined && p.default !== null
    const secret = !!p.secret
    let remembered: string | null = null
    let note: string | null = null
    if (secret) {
      note = 'a secret: never remembered, enter it at every start'
    } else if (recalled?.status.status === 'incompatible') {
      note = `not using the remembered value: ${incompatText(recalled.status.reason)}`
    } else if (typeof recalled?.value === 'string') {
      remembered = recalled.value
    }
    return {
      name: p.name,
      label: p.label || p.name,
      type: p.type,
      options: p.options ?? [],
      secret,
      value: secret ? '' : (remembered ?? fallback),
      fallback,
      hasDefault,
      remembered,
      memoryKnown: recall !== null,
      note,
      required: secret || !hasDefault,
    }
  })
}

// The fields after the memory answer arrived: every field the person has
// not touched yet (its value is still the one `base` prefilled) takes the
// remembered one; a field they typed in keeps what they typed.
export function mergeRecalled(current: ParamField[], base: ParamField[], recalled: ParamField[]): ParamField[] {
  return recalled.map((r) => {
    const now = current.find((c) => c.name === r.name)
    const before = base.find((b) => b.name === r.name)
    return now && before && now.value !== before.value ? { ...r, value: now.value } : r
  })
}

// The instruction the dialog starts with: what a start without one would
// use (remembered, else the playbook's old draft).
export function instructionFrom(recall: Recall | null): string {
  return recall?.instruction ?? ''
}

// What the dialog says about the memory it read: that the params changed
// since it was saved, and what the server found wrong with it.
export function memoryNoteFrom(recall: Recall): string | null {
  const notes = [
    ...(recall.schema_changed ? ['The params changed since the input was remembered.'] : []),
    ...recall.diagnostics,
  ]
  return notes.length ? notes.join(' ') : null
}

// The value field `f` sends, or null to leave it to the engine (see the
// rules at the top).
function sent(f: ParamField): string | null {
  if (f.value === '') {
    // Cleared, or memory unknown: the default, never a recalled value.
    if (f.remembered !== null || !f.memoryKnown) return f.hasDefault ? f.fallback : null
    return null
  }
  if (f.memoryKnown && f.value === f.fallback && f.remembered === null) return null
  return f.value
}

// The params, instruction and remember choice the start sends.
// `initial` is the instruction the dialog was prefilled with: one the person
// cleared is sent as an empty instruction, so the start does not fall back
// to the remembered one it no longer shows.
export function runInputFrom(
  fields: ParamField[],
  instruction: string,
  remember: boolean,
  initial = '',
): RunInput {
  const params: Record<string, string> = {}
  for (const f of fields) {
    const v = sent(f)
    if (v !== null) params[f.name] = v
  }
  const out: RunInput = { params }
  if (instruction.trim()) out.instruction = instruction
  else if (initial.trim()) out.instruction = ''
  if (remember) out.remember = true
  return out
}

// The names of params still without a value that the start needs.
export function missingValues(fields: ParamField[]): string[] {
  return fields.filter((f) => f.required && f.value === '').map((f) => f.name)
}
