//! Approval of an exact result snapshot set.
//!
//! A `human_review` gate with `approves:` binds each request to the latest
//! result snapshots of the nodes it names ([`ReviewBinding`]). An approving
//! decision must carry the binding's nonce, the same rule a publish consent
//! follows with its plan nonce, so it answers exactly the bytes the person
//! was shown. A node with `requires_approval:` is checked against that
//! approval before it starts (`scheduler::approval::check_before`); a
//! failure is one of [`ApprovalRefusal`].
//!
//! An unattended run's declared `decide` and an enforced recommendation
//! that choose an approving option satisfy `requires_approval` too,
//! labelled by `review_decided.decided_by`; a gate only a person may
//! approve is marked `person_only`.

use serde::{Deserialize, Serialize};

/// The one check every path that moves a run to a named node shares: a
/// supervisor patch's `continue_from`, `node_retry`, `run_continue_from`
/// and a resume's `from_node` (see `scheduler::approval`).
pub use crate::scheduler::approval::{refuse_resume_skip, resume_skips_gate, skips_gate};

/// The snapshots one request of a gate is bound to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewBinding {
    /// The binding nonce of this request ([`binding_nonce`]): an approving
    /// decision carries it.
    pub nonce: String,
    #[serde(default)]
    pub snapshots: Vec<BoundSnapshot>,
    /// Named nodes with no snapshot to bind; approving such a request does
    /// not approve them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<BindingMissing>>", optional))]
    pub missing: Vec<BindingMissing>,
}

/// One result snapshot a request is bound to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundSnapshot {
    pub node: String,
    /// The node's execution number (1-based) the snapshot was taken for.
    pub execution: u32,
    /// The list item, for an item snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<String>,
    /// The seq of the settlement event that stamped the snapshot.
    pub settlement_seq: u64,
    pub snapshot_id: String,
    /// Entries left out of the snapshot.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u32>", optional))]
    pub omitted: u32,
    /// The snapshot stopped short of what was declared.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub incomplete: bool,
    /// Some entry is stored redacted: it is approved as its redacted bytes
    /// and cannot be compared with the live file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub redacted: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A node `approves:` names that has nothing to bind.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingMissing {
    pub node: String,
    pub reason: BindingMissingReason,
}

/// Why a named node has nothing to bind.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingMissingReason {
    /// The node has not run in this run.
    NotRun,
    /// The node ran, but its latest execution has no snapshot.
    NoSnapshot,
}

impl BindingMissingReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingMissingReason::NotRun => "not_run",
            BindingMissingReason::NoSnapshot => "no_snapshot",
        }
    }
}

/// The binding nonce of a gate's request: over the run, the gate, the
/// request's visit number (the gate's request count plus one, known before
/// it is appended) and every bound and missing node. It changes with every
/// new request, so a decision queued for an earlier visit never answers a
/// later one, even when the snapshots are identical.
pub fn binding_nonce(
    run_id: &str,
    gate: &str,
    visit: u32,
    snapshots: &[BoundSnapshot],
    missing: &[BindingMissing],
) -> String {
    let mut bound: Vec<String> = snapshots
        .iter()
        .map(|s| format!("{}|{}|{}", s.node, s.settlement_seq, s.snapshot_id))
        .collect();
    bound.sort();
    let mut absent: Vec<String> = missing
        .iter()
        .map(|m| format!("missing:{}|{}", m.node, m.reason.as_str()))
        .collect();
    absent.sort();
    let mut lines = vec![
        "approval-v1".to_string(),
        run_id.to_string(),
        gate.to_string(),
        format!("visit={visit}"),
    ];
    lines.extend(bound);
    lines.extend(absent);
    apb_core::content::short_nonce("approval", &lines)
}

/// Why a node with `requires_approval` may not start, or a publish plan
/// does not match what was approved. A closed list every surface renders.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRefusal {
    /// The gate has no approving decision with a binding covering the
    /// required nodes.
    ApprovalMissing,
    /// A required node ran again after the approval, or a bound live file
    /// changed.
    ApprovalStale,
    /// A bound snapshot no longer verifies.
    ApprovalSnapshotUnreadable,
    /// A bound snapshot is incomplete.
    ApprovalIncomplete,
    /// A bound file was stored redacted, so the live file cannot be
    /// compared.
    ApprovalUnverifiable,
    /// The commit to push is not the commit the approved result was taken
    /// at.
    ApprovalMismatch,
}

impl ApprovalRefusal {
    /// Every refusal, in declaration order.
    pub const ALL: [ApprovalRefusal; 6] = [
        ApprovalRefusal::ApprovalMissing,
        ApprovalRefusal::ApprovalStale,
        ApprovalRefusal::ApprovalSnapshotUnreadable,
        ApprovalRefusal::ApprovalIncomplete,
        ApprovalRefusal::ApprovalUnverifiable,
        ApprovalRefusal::ApprovalMismatch,
    ];

    /// The wire name (`approval_stale`).
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalRefusal::ApprovalMissing => "approval_missing",
            ApprovalRefusal::ApprovalStale => "approval_stale",
            ApprovalRefusal::ApprovalSnapshotUnreadable => "approval_snapshot_unreadable",
            ApprovalRefusal::ApprovalIncomplete => "approval_incomplete",
            ApprovalRefusal::ApprovalUnverifiable => "approval_unverifiable",
            ApprovalRefusal::ApprovalMismatch => "approval_mismatch",
        }
    }

    /// The refusal in words, the same on every surface.
    pub fn sentence(self) -> &'static str {
        match self {
            ApprovalRefusal::ApprovalMissing => {
                "The gate has no approving decision that covers the nodes this one needs approved."
            }
            ApprovalRefusal::ApprovalStale => {
                "A required node ran again, or an approved file changed, after the approval."
            }
            ApprovalRefusal::ApprovalSnapshotUnreadable => {
                "An approved snapshot no longer verifies, so nothing is decided on it."
            }
            ApprovalRefusal::ApprovalIncomplete => {
                "An approved snapshot is incomplete: what was not stored was not approved."
            }
            ApprovalRefusal::ApprovalUnverifiable => {
                "An approved file was stored redacted, so the live file cannot be compared with it."
            }
            // The publish plan's own refusal, so both say it alike.
            ApprovalRefusal::ApprovalMismatch => {
                apb_core::publish::Refusal::ApprovalMismatch.sentence()
            }
        }
    }
}

/// A failed approval check: the refusal and what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalFailure {
    pub refusal: ApprovalRefusal,
    pub detail: String,
}

impl std::fmt::Display for ApprovalFailure {
    /// `<code>: <detail>`, the output a refused node finishes with.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.refusal.as_str(), self.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(node: &str, settlement_seq: u64, snapshot_id: &str) -> BoundSnapshot {
        BoundSnapshot {
            node: node.into(),
            execution: 1,
            item: None,
            settlement_seq,
            snapshot_id: snapshot_id.into(),
            omitted: 0,
            incomplete: false,
            redacted: false,
        }
    }

    /// The nonce input is fixed: the expected values were computed outside
    /// apb (sha256 over the newline-joined lines, first 32 hex digits), so a
    /// change to the line format, the sort or the prefix fails here. The
    /// order the snapshots arrive in does not matter; the visit does.
    #[test]
    fn binding_nonce_is_pinned_order_free_and_changes_with_the_visit() {
        let missing = [BindingMissing {
            node: "x".into(),
            reason: BindingMissingReason::NotRun,
        }];
        let a = [bound("w", 9, "ab"), bound("a", 12, "cd")];
        let b = [bound("a", 12, "cd"), bound("w", 9, "ab")];
        assert_eq!(
            binding_nonce("r1", "g", 2, &a, &missing),
            "approval-3fcd30582598bd5fa956cc2f0ea538ed"
        );
        assert_eq!(
            binding_nonce("r1", "g", 2, &b, &missing),
            "approval-3fcd30582598bd5fa956cc2f0ea538ed"
        );
        assert_eq!(
            binding_nonce("r1", "g", 3, &a, &missing),
            "approval-725f0147b40271826a24e9dd8087f63f"
        );
    }
}
