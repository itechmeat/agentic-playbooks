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
// The playbook trash (`GET /api/trash`, `POST /api/trash/{name}/restore`).
export type { RestoredPlaybook, TrashListEntry } from './api.gen'
// The trust store (`GET /api/trust`, `POST /api/trust/revoke`).
export type { OriginKind, TrustEntry, TrustKind, TrustRevoked } from './api.gen'

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
  /** The definition equaled the current version: nothing was written. */
  unchanged?: boolean
}

export interface VersionProvenance {
  created_by: string
  run_id: string | null
  classification: string | null
  /** `next_runs` for a forward patch (issue #192). */
  scope?: string
  base_version?: string
  rationale?: string
  evidence?: string[]
  trial?: TrialRecord
}

/** How a candidate fared in its trial runs (issue #192). */
export interface TrialRecord {
  successes: number
  /** `promoted`, `rejected` or `superseded`; absent while on trial. */
  outcome?: string
  run_id?: string
  reason?: string
}

// Versions come from the API oldest first in semver order.
export interface VersionInfo {
  version: string
  /** `current` points here: the one source for the version in use. */
  is_current: boolean
  /** The `candidate` pointer names it: a forward patch on trial. */
  is_candidate?: boolean
  provenance: VersionProvenance | null
}
