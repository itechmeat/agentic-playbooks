//! What a fork's branch-failure policy and a refusing join did (issue #195),
//! as the run surfaces show it: MCP `run_status` (`branch_failures`) and the
//! `apb runs <id>` text lines. Read-only, from the journal alone.

use serde::Serialize;

use crate::event::{Event, EventPayload};

/// One `branch_failed`, `branch_cancelled` or `join_refused` record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BranchFailureEntry {
    pub seq: u64,
    /// The event type.
    pub kind: &'static str,
    pub node: String,
    /// A short readable line.
    pub detail: String,
}

/// Every fork-policy and join-refusal record of a run, in journal order.
pub fn entries(events: &[Event]) -> Vec<BranchFailureEntry> {
    events
        .iter()
        .filter_map(|e| {
            let (kind, node, detail) = match &e.payload {
                EventPayload::BranchFailed {
                    fork,
                    node,
                    policy,
                    target,
                } => (
                    "branch_failed",
                    node,
                    match target {
                        Some(t) => format!("fork `{fork}` {policy}: failed, the run goes to `{t}`"),
                        None => format!(
                            "fork `{fork}` {policy}: failed, the other branches are cancelled"
                        ),
                    },
                ),
                EventPayload::BranchCancelled {
                    fork,
                    node,
                    failed_node,
                } => (
                    "branch_cancelled",
                    node,
                    format!("fork `{fork}`: cancelled after `{failed_node}` failed"),
                ),
                EventPayload::JoinRefused { node, reason, .. } => {
                    ("join_refused", node, reason.clone())
                }
                _ => return None,
            };
            Some(BranchFailureEntry {
                seq: e.seq,
                kind,
                node: node.clone(),
                detail,
            })
        })
        .collect()
}
