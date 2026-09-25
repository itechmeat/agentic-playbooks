import YAML, { type Document } from 'yaml'
import type { PlaybookDetail } from './types'

export type PlaybookModel = PlaybookDetail['playbook']

export function parsePlaybook(text: string): { model?: PlaybookModel; error?: string } {
  let doc: unknown
  try {
    doc = YAML.parse(text)
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) }
  }

  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) {
    return { error: 'YAML root must be a mapping' }
  }

  const root = doc as Record<string, unknown>
  const id = typeof root.id === 'string' ? root.id : ''
  const name = typeof root.name === 'string' ? root.name : id

  if (!Array.isArray(root.nodes)) {
    return { error: 'nodes must be an array' }
  }
  if (!Array.isArray(root.edges)) {
    return { error: 'edges must be an array' }
  }

  const nodes: PlaybookModel['nodes'] = []
  for (const item of root.nodes) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) {
      return { error: 'each node must be a mapping' }
    }
    const n = item as Record<string, unknown>
    if (typeof n.id !== 'string' || typeof n.type !== 'string') {
      return { error: 'each node needs id and type' }
    }
    nodes.push({ ...n, id: n.id, type: n.type } as PlaybookModel['nodes'][number])
  }

  const edges: PlaybookModel['edges'] = []
  for (const item of root.edges) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) {
      return { error: 'each edge must be a mapping' }
    }
    const e = item as Record<string, unknown>
    if (typeof e.from !== 'string' || typeof e.to !== 'string') {
      return { error: 'each edge needs from and to' }
    }
    edges.push({
      from: e.from,
      to: e.to,
      ...(e.condition != null ? { condition: e.condition as PlaybookModel['edges'][number]['condition'] } : {}),
      ...(e.fallback != null ? { fallback: e.fallback as boolean } : {}),
    })
  }

  // Only `on_failure` is read: the editor renders the graph from this model,
  // and the failure marker depends on it. Everything else under `defaults` is
  // irrelevant to the canvas and stays untouched in the YAML, which remains
  // the source of truth on save.
  const rawDefaults = root.defaults
  const onFailure =
    rawDefaults && typeof rawDefaults === 'object' && !Array.isArray(rawDefaults)
      ? (rawDefaults as Record<string, unknown>).on_failure
      : undefined

  return {
    model: {
      id,
      name,
      nodes,
      edges,
      defaults: typeof onFailure === 'string' ? { on_failure: onFailure } : undefined,
    },
  }
}

/**
 * Parses YAML into a Document (the yaml package's AST), preserving comments
 * and order. Used for structural edits via wfedit (field and edge mutations)
 * so other top-level fields aren't lost. On a syntax error returns {error}.
 */
export function parseDoc(text: string): { doc?: Document; error?: string } {
  try {
    const doc = YAML.parseDocument(text)
    if (doc.errors.length) {
      return { error: doc.errors[0].message }
    }
    return { doc }
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) }
  }
}

/** Serializes a Document back into YAML text. */
export function docToString(doc: Document): string {
  return doc.toString()
}

/** Starter template for a new playbook: generated from apb-core, so it always
 * declares the schema this apb writes. */
export { NEW_PLAYBOOK_TEMPLATE } from './api.gen'
