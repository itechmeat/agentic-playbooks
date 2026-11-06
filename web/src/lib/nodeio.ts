// Node form helpers for the fields issue #67 added: declared output fields
// (`outputs.fields`) and the node a `continue_session` may name.

/** The name of one declared field entry: a plain string or a typed map. */
function entryName(f: unknown): string {
  if (f && typeof f === 'object' && 'name' in (f as Record<string, unknown>)) {
    return String((f as Record<string, unknown>).name)
  }
  return String(f)
}

/** The declared output fields as the form shows them: `a, b`. */
export function fieldsToField(outputs: unknown): string {
  const fields = (outputs as { fields?: unknown } | null | undefined)?.fields
  return Array.isArray(fields) ? fields.map(entryName).join(', ') : ''
}

/**
 * The node's `outputs` after the fields input changed to `raw`: the other
 * keys (`files`, `extract`) stay, an empty list drops `fields`, and an
 * `outputs` left with nothing drops out entirely.
 */
export function fieldToOutputs(
  raw: string,
  current: unknown,
): Record<string, unknown> | undefined {
  const names = raw
    .split(',')
    .map((f) => f.trim())
    .filter((f) => f !== '')
  const base: Record<string, unknown> = { ...((current as Record<string, unknown> | null) ?? {}) }
  delete base.fields
  const prev = Array.isArray((current as { fields?: unknown } | null | undefined)?.fields)
    ? ((current as { fields: unknown[] }).fields as unknown[])
    : []
  // A name that a typed entry still declares keeps that entry; a name the
  // user added (or one whose typed entry they removed) lands as a plain
  // string. The declaration follows the order the names were typed in.
  const fields = names.map((n) => prev.find((f) => entryName(f) === n) ?? n)
  if (fields.length) base.fields = fields
  return Object.keys(base).length ? base : undefined
}

/** The nodes `continue_session` of node `self` may name: the other agent_tasks. */
export function sessionSources(nodes: { id: string; type: string }[], self: string): string[] {
  return nodes.filter((n) => n.type === 'agent_task' && n.id !== self).map((n) => n.id)
}
