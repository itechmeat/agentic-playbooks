import type { DraftValidation, IssueView } from './api.gen'
import type { FlowEdge, FlowNode } from './graph'
import { issueLine } from './playbookedit'

// Validation issues on a graph: what each one points at, the markers on
// nodes and edges, the editor's "fix" link, and the editor's draft
// validation that keeps the last valid result while the YAML does not parse.

/** An issue as the playbook detail, the draft validation or a refused save sends it. */
export type AnyIssue = Pick<IssueView, 'code' | 'severity' | 'message'> & { node?: string | null; edges?: string[] }

/** What a canvas can show: its node ids and its edges' rendered addresses. */
export interface Drawable {
  nodes: ReadonlySet<string>
  edges: ReadonlySet<string>
}

/** The thing an issue is about, by its data. */
export type IssueTarget =
  /** Its edges present on the canvas, all highlighted; the first selected. */
  | { kind: 'edge'; edges: string[]; first: string }
  | { kind: 'node'; node: string }
  /** Nothing drawable; `yaml` when the YAML view is the place to look. */
  | { kind: 'document'; yaml: boolean }

/** V06 names an edge to a node that does not exist: nothing to draw. */
const YAML_ONLY = new Set(['V06'])

/**
 * Kind by data: `edges` non-empty is an edge issue, else a `node` naming a
 * drawable node is a node issue, else a document issue. Navigation goes
 * only to targets present on the canvas.
 */
export function issueTarget(issue: Pick<AnyIssue, 'code' | 'node' | 'edges'>, drawable: Drawable): IssueTarget {
  if (YAML_ONLY.has(issue.code)) return { kind: 'document', yaml: true }
  const edges = (issue.edges ?? []).filter((e) => drawable.edges.has(e))
  if (edges.length) return { kind: 'edge', edges, first: edges[0] }
  if (issue.node && drawable.nodes.has(issue.node)) return { kind: 'node', node: issue.node }
  return { kind: 'document', yaml: false }
}

/** The canvas of a flow as a `Drawable`. */
export function drawableOf(nodes: Pick<FlowNode, 'id'>[], edges: Pick<FlowEdge, 'data'>[]): Drawable {
  return {
    nodes: new Set(nodes.map((n) => n.id)),
    edges: new Set(edges.map((e) => e.data.address).filter((a): a is string => !!a)),
  }
}

type Marker = { text: string; error: boolean }

function add(m: Map<string, Marker>, key: string, issue: AnyIssue) {
  const prev = m.get(key)
  const line = `${issue.severity} ${issueLine(issue)}`
  m.set(key, {
    text: prev ? `${prev.text}\n${line}` : line,
    error: (prev?.error ?? false) || issue.severity === 'error',
  })
}

/** The marker of each node and edge the issues point at. */
export function issueMarkers(
  issues: AnyIssue[],
  drawable: Drawable,
): { nodes: Map<string, Marker>; edges: Map<string, Marker> } {
  const nodes = new Map<string, Marker>()
  const edges = new Map<string, Marker>()
  for (const i of issues) {
    const t = issueTarget(i, drawable)
    if (t.kind === 'edge') for (const e of t.edges) add(edges, e, i)
    else if (t.kind === 'node') add(nodes, t.node, i)
  }
  return { nodes, edges }
}

/**
 * The flow with its issue markers set (and stale ones cleared), and the
 * edges of `highlight` drawn as the selected issue's targets.
 */
export function withIssues(
  nodes: FlowNode[],
  edges: FlowEdge[],
  issues: AnyIssue[],
  highlight: ReadonlySet<string> = new Set(),
): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const m = issueMarkers(issues, drawableOf(nodes, edges))
  return {
    nodes: nodes.map((n) => {
      const issue = m.nodes.get(n.id)
      if (!issue && !n.data.issue) return n
      const { issue: _old, ...data } = n.data
      return { ...n, data: issue ? { ...data, issue } : data }
    }),
    edges: edges.map((e) => {
      const address = e.data.address ?? ''
      const issue = m.edges.get(address)
      const lit = highlight.has(address)
      if (!issue && !e.data.issue && !lit && !e.data.highlight) return e
      const { issue: _old, highlight: _h, ...data } = e.data
      return { ...e, data: { ...data, ...(issue ? { issue } : {}), ...(lit ? { highlight: true } : {}) } }
    }),
  }
}

/** The editor link that opens an issue's target ("fix"). */
export function fixHash(workspace: string, id: string, target: IssueTarget): string {
  const base = `#/edit/${encodeURIComponent(workspace)}/${encodeURIComponent(id)}`
  if (target.kind === 'edge') return `${base}?edge=${encodeURIComponent(target.first)}`
  if (target.kind === 'node') return `${base}?node=${encodeURIComponent(target.node)}`
  return target.yaml ? `${base}?yaml=1` : base
}

/** The editor's issue list: the latest valid validation, stale after a parse error. */
export interface DraftIssues {
  issues: IssueView[]
  /** The issues are from the last valid YAML; the current text does not parse. */
  stale: boolean
  parseError: string | null
}

/**
 * The list after a draft validation: a parse error keeps the last valid
 * issues, marked as from the last valid YAML, with the parse error as a
 * document issue of its own.
 */
export function nextDraftIssues(prev: DraftIssues, result: DraftValidation): DraftIssues {
  if (result.parse_error) return { issues: prev.issues, stale: true, parseError: result.parse_error }
  return { issues: result.issues, stale: false, parseError: null }
}

/** The parse error as a document issue. */
export function parseIssue(message: string): IssueView {
  return { code: 'parse', severity: 'error', message, edges: [] }
}
