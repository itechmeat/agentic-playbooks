//! The logical identity of a settled result: which execution (and which
//! list item) settled it, at which journal seq, which part of it is meant,
//! and the attempt that produced it.
//!
//! `settlement_seq` is the seq of the `node_finished` or `list_item_settled`
//! the result belongs to, so a later settlement of the same item or node is
//! a different result. Pure: every reference is derived from the journal
//! ([`crate::identity`]), nothing is stored.

use serde::Serialize;

use crate::identity::{ExecutionRef, ListVisitRef};

/// One settled result.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResultRef {
    pub execution: ExecutionRef,
    /// The list item, for a result one item settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub item: Option<ResultItem>,
    pub settlement_seq: u64,
    pub part: ResultPart,
    /// The attempt id (the seq of its `attempt_started`) that produced the
    /// result; absent for work that spawned no attempt (a finish without a
    /// prompt, a sub-playbook node, a cache hit).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub produced_by_attempt: Option<u64>,
}

/// A list item: its visit and its id.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResultItem {
    pub visit: ListVisitRef,
    pub id: String,
}

/// Which part of a settled result is meant.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultPart {
    /// The whole output text.
    Output,
    /// The output's named fields.
    Fields,
    /// A declared file (results snapshots; not produced yet).
    File { scope: String, path: String },
}
