//! A list node's visit as the run surfaces show it: every item's state and
//! the counts ([`RunList`]), or the counts alone ([`ListProgress`]).

use serde::Serialize;

use apb_core::list::verdict::ItemState;

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
    /// Its check rejected it in some round of the visit, whatever came after.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub check_rejected: bool,
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

impl RunList {
    /// The list with its counts whole but only its first `cap` items that
    /// need a look, for a status read that stays small on a long list.
    /// This list with its first `cap` items, and how many it left out.
    pub fn first_items(&self, cap: usize) -> (RunList, usize) {
        let list = RunList {
            items: self.items.iter().take(cap).cloned().collect(),
            ..self.clone()
        };
        (list, self.items.len().saturating_sub(cap))
    }

    pub fn needing_a_look(&self, cap: usize) -> RunList {
        RunList {
            items: self
                .items
                .iter()
                .filter(|i| i.state == ItemState::NeedsLook.as_str())
                .take(cap)
                .cloned()
                .collect(),
            ..self.clone()
        }
    }
}

impl RunListItem {
    fn of(id: &str, r: &ItemRecord) -> RunListItem {
        RunListItem {
            id: id.to_string(),
            state: r.state.map_or("pending", ItemState::as_str).to_string(),
            round: r.round,
            verdict: r.verdict.clone(),
            note: r.note.clone(),
            reason: r.reason.clone(),
            attempts: r.attempts,
            check_rejected: r.check_rejected,
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

/// Every list node that started a visit, as [`RunList`]s: in playbook
/// order with their titles, or, without a readable playbook snapshot, in
/// the order the journal started them and without titles. Every surface
/// that counts a run's list items reads them here, so a listing and a run's
/// detail never disagree.
pub fn run_lists(
    playbook: Option<&apb_core::schema::Playbook>,
    events: &[crate::event::Event],
) -> Vec<RunList> {
    let nodes: Vec<(&str, Option<&str>)> = match playbook {
        Some(p) => p
            .nodes
            .iter()
            .filter(|n| n.list().is_some())
            .map(|n| (n.id.as_str(), n.title.as_deref()))
            .collect(),
        None => {
            let mut started: Vec<(&str, Option<&str>)> = Vec::new();
            for e in events {
                if let crate::event::EventPayload::ListStarted { node, .. } = &e.payload
                    && !started.iter().any(|(n, _)| n == node)
                {
                    started.push((node.as_str(), None));
                }
            }
            started
        }
    };
    nodes
        .into_iter()
        .filter_map(|(id, title)| ListFold::of(events, id).map(|f| RunList::of(&f, title)))
        .collect()
}
