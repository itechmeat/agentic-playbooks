// Node form helpers for the fields issue #67 added: declared output fields
// (`outputs.fields`) and the node a `continue_session` may name.

/** The declared output fields as the form shows them: `a, b`. */
export function fieldsToField(outputs: unknown): string {
  const fields = (outputs as { fields?: unknown } | null | undefined)?.fields
  return Array.isArray(fields) ? fields.map(String).join(', ') : ''
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
  const fields = raw
    .split(',')
    .map((f) => f.trim())
    .filter((f) => f !== '')
  const base: Record<string, unknown> = { ...((current as Record<string, unknown> | null) ?? {}) }
  delete base.fields
  if (fields.length) base.fields = fields
  return Object.keys(base).length ? base : undefined
}

/** The nodes `continue_session` of node `self` may name: the other agent_tasks. */
export function sessionSources(nodes: { id: string; type: string }[], self: string): string[] {
  return nodes.filter((n) => n.type === 'agent_task' && n.id !== self).map((n) => n.id)
}
