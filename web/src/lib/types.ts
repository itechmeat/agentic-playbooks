export interface Project {
  workspace_id: string
  name: string
  path: string
  playbook_count: number
}

export interface PlaybookSummary {
  id: string
  name: string
  description: string
  current: string
  versions: string[]
  frozen: boolean
  // Owning project (global dashboard). Empty on the pinned-root test server.
  workspace_id: string
  project: string
}

export interface PlaybookNode {
  id: string
  type: string
  title?: string | null
  [key: string]: unknown
}

export interface PlaybookEdge {
  from: string
  to: string
  condition?: { type: string; [key: string]: unknown } | null
  fallback?: boolean
}

export interface LayoutNode { id: string; x: number; y: number }

// `userArranged` is set by the editor once the user drags a node by hand. It
// is optional everywhere it is parsed: absent means "auto-laid-out" and keeps
// every existing call site backward compatible. The server stores and loads
// the layout as an opaque value, so the field survives the round-trip with no
// backend change.
export interface WfLayout { nodes?: LayoutNode[]; userArranged?: boolean }

export interface PlaybookDetail {
  id: string
  version: string
  yaml: string
  playbook: {
    id: string
    name: string
    nodes: PlaybookNode[]
    edges: PlaybookEdge[]
    // Only the field the graph needs: what an unhandled failure does -
    // `route`, `stop`, or the id of the node it goes to. Absent (and on any
    // playbook written before the policy existed) means `route`.
    defaults?: { on_failure?: string } | null
  }
  layout: WfLayout | null
  validation: { code: string; severity: string; message: string; node?: string | null }[]
  frozen: boolean
}

// Run payloads (detail, listing, progress and every open gate) are generated
// from the Rust types the server serializes, so the two cannot drift. See
// crates/apb-server/src/ts_contract.rs.
export type {
  ChildRun,
  NodeStatus,
  PendingQuestion,
  PendingReview,
  PendingSupervisor,
  ProgressSummary,
  RunDetail,
  RunStatus,
  WaitingKind,
} from './api.gen'
export type { RunListEntry as RunSummary } from './api.gen'

export interface WfEvent {
  seq: number
  ts: number
  type: string
  node?: string | null
  trigger?: string
  action?: string
  detail?: string
  [key: string]: unknown
}

export interface VersionDiff {
  nodes_added: string[]
  nodes_removed: string[]
  nodes_changed: string[]
  edges_added: string[]
  edges_removed: string[]
  yaml_diff: string
}

export interface WriteResult {
  id: string
  version: string
}

export interface VersionProvenance {
  created_by: string
  run_id: string | null
  classification: string | null
}

// Versions come from the API oldest first in semver order.
export interface VersionInfo {
  version: string
  /** `current` points here: the one source for the version in use. */
  is_current: boolean
  provenance: VersionProvenance | null
}
