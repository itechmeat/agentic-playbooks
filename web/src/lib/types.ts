import type { BudgetLimit, OnNoEstimate } from './api.gen'

// A run's budget argument: the generated limit plus the policy.
export type RunBudget = BudgetLimit & { on_no_estimate?: OnNoEstimate }

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
  // A `list:` block: the node runs its step once per item. Only its
  // presence matters to the graph.
  list?: Record<string, unknown> | null
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

// One run input of a playbook (`params:`). A secret param is asked at every
// start and never remembered or prefilled.
export interface PlaybookParam {
  name: string
  type: string
  label?: string | null
  options?: string[] | null
  default?: unknown
  secret?: boolean
}

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
    // The playbook's run budget, the Run dialog's defaults.
    budget?: RunBudget | null
    // The run inputs the Run dialog asks for.
    params?: PlaybookParam[]
    // Every agent node must run with isolation full (shown read-only).
    publish_only?: boolean
  }
  layout: WfLayout | null
  validation: { code: string; severity: string; message: string; node?: string | null }[]
  frozen: boolean
}

// Run payloads (detail, listing, progress and every open gate) are generated
// from the Rust types the server serializes, so the two cannot drift. See
// crates/apb-server/src/ts_contract.rs.
export type {
  BudgetStatus,
  ChildRun,
  NodeStatus,
  OnNoEstimate,
  RunEstimate,
  PauseBy,
  PauseInfo,
  PauseMode,
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

// Version history: generated from the Rust types the server serializes.
export type {
  ImportProvenance,
  TrialRecord,
  VersionInfo,
  VersionProvenance,
} from './api.gen'
