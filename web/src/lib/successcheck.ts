// A node's success_check in YAML - a bare string (script path under scripts/)
// OR a map `{ marker: "<literal>" }` (completion marker in node output).
// Separate module so both forms round-trip through the NodePanel without
// coercing a marker object into "[object Object]" or a plain string.

export type SuccessCheckMode = 'script' | 'marker'

/** Value written into playbook YAML for the success_check field. */
export type SuccessCheck = string | { marker: string }

export function isSuccessCheckMode(v: string | undefined | null): v is SuccessCheckMode {
  return v === 'script' || v === 'marker'
}

export function successCheckMode(v: unknown): SuccessCheckMode {
  if (v && typeof v === 'object' && typeof (v as { marker?: unknown }).marker === 'string') {
    return 'marker'
  }
  return 'script'
}

/** Stored YAML value -> UI mode + editable text. */
export function successCheckToFields(v: unknown): { mode: SuccessCheckMode; value: string } {
  if (typeof v === 'string') return { mode: 'script', value: v }
  if (v && typeof v === 'object') {
    const marker = (v as { marker?: unknown }).marker
    if (typeof marker === 'string') return { mode: 'marker', value: marker }
  }
  return { mode: 'script', value: '' }
}

/**
 * UI mode + text -> value to write into YAML. Empty text clears the field
 * (undefined) so an absent check stays absent.
 */
export function fieldsToSuccessCheck(
  mode: SuccessCheckMode,
  value: string,
): SuccessCheck | undefined {
  const trimmed = value.trim()
  if (trimmed === '') return undefined
  if (mode === 'marker') return { marker: trimmed }
  return trimmed
}
