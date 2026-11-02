//! A list node's visit as the run surfaces show it: every item's state and
//! the counts ([`RunList`]), or the counts alone ([`ListProgress`]).

use std::collections::BTreeSet;

use serde::Serialize;

use apb_core::list::verdict::ItemState;

use super::ledger::{ItemRecord, ListFold};
use crate::connector::intent::{Ledger, holds_send_open};

/// What the row of a passed item with an earlier send still unknown says
/// beside its state ([`RunListItem::unresolved_send`]), on every surface.
pub const UNRESOLVED_SEND_TEXT: &str = "earlier send unresolved";

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
    /// It passed while an earlier send of it is still unknown: the item
    /// reads its send chain, which a later resend settled, while that first
    /// send stays open until a person resolves it. Only a passed item says
    /// so; an item held by an unknown send needs a look instead.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub unresolved_send: bool,
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
        Self::with_open_sends(fold, title, &BTreeSet::new())
    }

    /// [`RunList::of`] with `open_sends`, the items of the node that hold a
    /// send open for a person ([`open_send_items`]).
    fn with_open_sends(
        fold: &ListFold,
        title: Option<&str>,
        open_sends: &BTreeSet<String>,
    ) -> RunList {
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
                .map(|(id, r)| RunListItem::of(id, r, open_sends.contains(id)))
                .collect(),
        }
    }
}

impl RunList {
    /// This list with its first `cap` items, and how many it left out.
    pub fn first_items(&self, cap: usize) -> (RunList, usize) {
        let list = RunList {
            items: self.items.iter().take(cap).cloned().collect(),
            ..self.clone()
        };
        (list, self.items.len().saturating_sub(cap))
    }

    /// The list with its counts whole but only its first `cap` items a
    /// person looks at (those that need a look, and those that passed with
    /// an earlier send still unknown), and how many such items it left out:
    /// a status read that stays small on a long list.
    pub fn needing_a_look(&self, cap: usize) -> (RunList, usize) {
        let looks: Vec<&RunListItem> = self.items.iter().filter(|i| i.wants_a_look()).collect();
        let list = RunList {
            items: looks.iter().take(cap).map(|i| (*i).clone()).collect(),
            ..self.clone()
        };
        (list, looks.len().saturating_sub(cap))
    }
}

impl RunListItem {
    /// Whether a person looks at this item: it needs a look, or it passed
    /// with an earlier send still unknown.
    pub fn wants_a_look(&self) -> bool {
        self.state == ItemState::NeedsLook.as_str() || self.unresolved_send
    }

    fn of(id: &str, r: &ItemRecord, open_send: bool) -> RunListItem {
        RunListItem {
            unresolved_send: open_send && r.state == Some(ItemState::Passed),
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
    let ledger = Ledger::fold(events);
    let now_ms = apb_core::clock::now_ms_u64();
    nodes
        .into_iter()
        .filter_map(|(id, title)| {
            ListFold::of(events, id)
                .map(|f| RunList::with_open_sends(&f, title, &open_send_items(&ledger, id, now_ms)))
        })
        .collect()
}

/// The items of the list node `node` with a send nobody can tell about,
/// open for a person ([`holds_send_open`]).
fn open_send_items(ledger: &Ledger, node: &str, now_ms: u64) -> BTreeSet<String> {
    ledger
        .records()
        .iter()
        .filter(|r| r.key.node == node)
        .filter_map(|r| {
            let item = r.key.item.as_ref()?;
            holds_send_open(ledger, r, now_ms).then(|| item.clone())
        })
        .collect()
}
