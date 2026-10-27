//! A list node's visit as the run surfaces show it: every item's state and
//! the counts ([`RunList`]), or the counts alone ([`ListProgress`]).

use serde::Serialize;

use super::ledger::{ItemRecord, ListFold};

/// One list node's visit with its items.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunList {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    pub total: u32,
    pub processed: u32,
    pub passed: u32,
    pub passed_with_note: u32,
    pub needs_look: u32,
    pub pending: u32,
    pub running: u32,
    /// The batch running or last run (from 1).
    pub batch: u32,
    /// How many batches the visit needs at its batch size.
    pub batches: u32,
    pub items: Vec<RunListItem>,
}

/// One item of a [`RunList`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunListItem {
    pub id: String,
    /// `pending`, `running`, `passed` or `needs_look`.
    pub state: String,
    pub round: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
    /// Why it needs a look.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
    pub attempts: u32,
}

/// A list node's counts, for the progress summary.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListProgress {
    pub node: String,
    pub total: u32,
    pub processed: u32,
    pub passed: u32,
    pub needs_look: u32,
    pub running: u32,
}

impl RunList {
    /// The view of `fold`: the items the visit has a record of, in id
    /// order. An item never admitted yet has no row; `pending` counts it.
    pub fn of(fold: &ListFold, title: Option<&str>) -> RunList {
        let c = fold.counts();
        let batches = match fold.batch_size {
            0 => 0,
            size => fold.total.div_ceil(size),
        };
        RunList {
            node: fold.node.clone(),
            title: title.map(str::to_string),
            total: c.total,
            processed: c.processed,
            passed: c.passed,
            passed_with_note: c.passed_with_note,
            needs_look: c.needs_look,
            pending: c.pending,
            running: c.running,
            batch: fold.batch,
            batches: batches.max(fold.batch),
            items: fold
                .items
                .iter()
                .map(|(id, r)| RunListItem::of(id, r))
                .collect(),
        }
    }
}

impl RunListItem {
    fn of(id: &str, r: &ItemRecord) -> RunListItem {
        RunListItem {
            id: id.to_string(),
            state: r
                .state
                .map_or("pending", apb_core::list::verdict::ItemState::as_str)
                .to_string(),
            round: r.round,
            verdict: r.verdict.clone(),
            note: r.note.clone(),
            reason: r.reason.clone(),
            attempts: r.attempts,
        }
    }
}

impl ListProgress {
    /// The counts of `fold`.
    pub fn of(fold: &ListFold) -> ListProgress {
        let c = fold.counts();
        ListProgress {
            node: fold.node.clone(),
            total: c.total,
            processed: c.processed,
            passed: c.passed,
            needs_look: c.needs_look,
            running: c.running,
        }
    }
}

/// Every list node of `playbook` that started a visit, as [`RunList`]s in
/// playbook order.
pub fn run_lists(
    playbook: &apb_core::schema::Playbook,
    events: &[crate::event::Event],
) -> Vec<RunList> {
    playbook
        .nodes
        .iter()
        .filter(|n| n.list().is_some())
        .filter_map(|n| ListFold::of(events, &n.id).map(|f| RunList::of(&f, n.title.as_deref())))
        .collect()
}
