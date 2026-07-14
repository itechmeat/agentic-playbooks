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

  return { model: { id, name, nodes, edges } }
}

// NOTE (for Task 3, structural edits): this function serializes ONLY
// id/name/nodes/edges - the PlaybookModel type doesn't contain other fields. A
// round trip through parsePlaybook -> serializePlaybook WILL LOSE
// schema/version/params/executors/defaults/supervisor. This is currently safe:
// the editor keeps YAML as the source of truth and sends raw text on save,
// serializePlaybook is not involved here. Before Task 3 starts writing
// structural edits back into YAML, either the model needs to be extended to
// cover all top-level fields, or edits need to go through the YAML document's
// AST from the `yaml` package (Document), preserving the other fields.
export function serializePlaybook(model: PlaybookModel): string {
  return YAML.stringify({
    id: model.id,
    name: model.name,
    nodes: model.nodes,
    edges: model.edges,
  })
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

/** Starter template for a new playbook. */
export const NEW_PLAYBOOK_TEMPLATE = `schema: 1
id: new-playbook
name: New Playbook
version: 0.1.0

nodes:
  - id: start
    type: start
    title: Start
  - id: done
    type: finish
    outcome: success

edges:
  - { from: start, to: done }
`
