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

export interface PlaybookDetail {
  id: string
  version: string
  yaml: string
  playbook: { id: string; name: string; nodes: PlaybookNode[]; edges: PlaybookEdge[] }
  layout: { nodes?: LayoutNode[] } | null
  validation: { code: string; severity: string; message: string; node?: string | null }[]
  frozen: boolean
}

// The pending question for a run parked on an interactive `agent_task` node
// (spec 2026-07-20-interactive-nodes), mirroring the server's
// `progress::PendingQuestion`. Present only while `waiting_kind === 'question'`.
export interface PendingQuestion {
  node: string
  question: string
  options: string[]
  // "human" or "supervisor" (the node's declared answer_by); the web facade
  // always posts as "human" regardless of this value.
  answer_by: string
  // Milliseconds since epoch, or 0 before drive has journaled the question
  // event yet (treat 0 as "just now", never synthesize a client-side time).
  asked_at: number
}

export interface ProgressSummary {
  percent: number
  label: string | null
  waiting_on: string | null
  // null whenever waiting_on is null.
  waiting_kind: 'human_review' | 'wait' | 'question' | null
  // The pending question when waiting_kind === 'question'; null otherwise.
  pending_question?: PendingQuestion | null
  // Deterministic work-plan identity: changes exactly when a report raises a
  // cycle total or the run migrates to a patched version. Does not change on
  // ordinary done/label updates. This is the only valid reset signal.
  plan_key: string
}

export interface RunSummary {
  run_id: string
  playbook: string
  status: string
  started_ts: number
  // Owning project (global dashboard). Empty on the pinned-root test server.
  workspace_id: string
  project: string
  progress?: ProgressSummary | null
  parent_run?: string | null
  continued_from?: string | null
  superseded_by?: string | null
}

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
  promoted: boolean
}

export interface VersionInfo {
  version: string
  is_current: boolean
  provenance: VersionProvenance | null
}

export interface RunDetail {
  run_id: string
  playbook: string
  version: string
  run_status: string
  nodes: Record<string, string>
  outputs: Record<string, string>
  instruction: string | null
  answer?: string | null
  params: Record<string, string>
  model: { id: string; name: string; nodes: PlaybookNode[]; edges: PlaybookEdge[] } | null
  layout: { nodes?: LayoutNode[] } | null
  hooks?: Record<string, string>
  events: WfEvent[]
  progress?: ProgressSummary | null
  // Sub-runs started by a `playbook` node in this run (review R1-I6), one
  // entry per `ChildRunStarted` event. Empty (not absent) when there are
  // none; `status` is folded from the child run's own event log, `"unknown"`
  // if that log could not be read.
  children?: { node_id: string; run_id: string; status: string }[]
}
