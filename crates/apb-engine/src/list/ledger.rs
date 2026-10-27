//! The journal's record of a list node's visit: every item's state, round,
//! verdict and note, folded from `list_started`, `list_batch_started`,
//! `list_item_settled` and the item attempts. The one reader resume, a
//! supervisor retry, `rerun: not_passed`, progress and every run surface
//! share.

use std::collections::BTreeMap;

use apb_core::list::verdict::ItemState;

use crate::event::{Event, EventPayload};

/// `list_item_settled.state` values.
pub mod settled {
    pub const PASSED: &str = "passed";
    /// Rejected by its check; it runs again in the round the event names.
    pub const REJECTED: &str = "rejected";
    pub const NEEDS_LOOK: &str = "needs_look";
    /// Back to pending without spending a round.
    pub const INTERRUPTED: &str = "interrupted";
}

/// The `reason` of an `interrupted` settlement that requeues an item for
/// its free reconcile run.
pub const RECONCILE: &str = "reconcile";

/// One item as the visit recorded it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemRecord {
    pub state: Option<ItemState>,
    /// The round it is in (0 for its first run).
    pub round: u32,
    pub verdict: Option<String>,
    pub note: Option<String>,
    pub reason: Option<String>,
    /// Compact JSON of its declared outputs, clipped.
    pub output: Option<String>,
    /// Attempts its executions started in this visit.
    pub attempts: u32,
    pub carried: bool,
    /// It had its free reconcile run.
    pub reconciled: bool,
    /// The batch it was last admitted in.
    pub batch: u32,
    /// The digest of the content it was settled on.
    pub digest: Option<String>,
    /// Its check rejected it in some round of this visit.
    pub check_rejected: bool,
}

impl ItemRecord {
    pub fn is_terminal(&self) -> bool {
        self.state.is_some_and(ItemState::is_terminal)
    }

    pub fn passed_with_note(&self) -> bool {
        self.state == Some(ItemState::Passed) && self.verdict.as_deref() == Some("accept_with_note")
    }
}

/// Counts over a visit's items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ListCounts {
    pub total: u32,
    pub processed: u32,
    pub passed: u32,
    pub passed_with_note: u32,
    pub needs_look: u32,
    pub pending: u32,
    pub running: u32,
}

/// One visit of a list node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListFold {
    pub node: String,
    /// The seq of the visit's first `list_started`.
    pub started_seq: u64,
    pub items_digest: String,
    pub total: u32,
    pub filtered_out: u32,
    pub batch_size: u32,
    pub parallel: u32,
    /// The last batch started (from 1).
    pub batch: u32,
    pub items: BTreeMap<String, ItemRecord>,
    /// A `node_finished` of the node follows the visit's last
    /// `list_started`.
    pub finished: bool,
    /// The status of that `node_finished`.
    pub finished_status: Option<String>,
}

fn list_started<'a>(e: &'a Event, node: &str) -> Option<&'a EventPayload> {
    match &e.payload {
        p @ EventPayload::ListStarted { node: n, .. } if n == node => Some(p),
        _ => None,
    }
}

fn is_reopen(p: &EventPayload) -> bool {
    matches!(p, EventPayload::ListStarted { reopened: true, .. })
}

/// Index of every visit start of `node`: a `list_started` that is not a
/// reopen (a reopen continues the visit it reopens).
fn visit_starts(events: &[Event], node: &str) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| list_started(e, node).is_some_and(|p| !is_reopen(p)))
        .map(|(i, _)| i)
        .collect()
}

impl ListFold {
    /// The latest visit of `node`, `None` when it never started one.
    pub fn of(events: &[Event], node: &str) -> Option<ListFold> {
        let start = *visit_starts(events, node).last()?;
        Some(Self::fold(events, node, start, events.len()))
    }

    /// The visit before the latest one, for `rerun: not_passed`.
    pub fn previous(events: &[Event], node: &str) -> Option<ListFold> {
        let starts = visit_starts(events, node);
        let n = starts.len();
        (n >= 2).then(|| Self::fold(events, node, starts[n - 2], starts[n - 1]))
    }

    fn fold(events: &[Event], node: &str, from: usize, until: usize) -> ListFold {
        let mut f = ListFold {
            node: node.to_string(),
            started_seq: events[from].seq,
            ..ListFold::default()
        };
        for e in &events[from..until] {
            match &e.payload {
                EventPayload::ListStarted {
                    node: n,
                    total,
                    filtered_out,
                    items_digest,
                    batch_size,
                    parallel,
                    ..
                } if n == node => {
                    f.total = *total;
                    f.filtered_out = *filtered_out;
                    f.items_digest = items_digest.clone();
                    f.batch_size = *batch_size;
                    f.parallel = *parallel;
                    f.finished = false;
                    f.finished_status = None;
                }
                EventPayload::ListBatchStarted {
                    node: n,
                    batch,
                    items,
                } if n == node => {
                    f.batch = *batch;
                    for id in items {
                        let r = f.items.entry(id.clone()).or_default();
                        if !r.is_terminal() {
                            r.state = Some(ItemState::Running);
                        }
                        r.batch = *batch;
                    }
                }
                EventPayload::ListItemSettled {
                    node: n,
                    item,
                    state,
                    verdict,
                    note,
                    reason,
                    output,
                    round,
                    carried,
                    digest,
                    ..
                } if n == node => {
                    let r = f.items.entry(item.clone()).or_default();
                    r.digest = digest.clone();
                    r.round = *round;
                    r.carried = *carried;
                    if verdict.is_some() {
                        r.verdict = verdict.clone();
                    }
                    // A passed item shows its final note only; a rejection's
                    // note stays in the journal, not in the record.
                    if note.is_some() || state == settled::PASSED {
                        r.note = note.clone();
                    }
                    r.reason = reason.clone();
                    r.check_rejected |= state == settled::REJECTED;
                    if output.is_some() {
                        r.output = output.clone();
                    }
                    r.state = Some(match state.as_str() {
                        settled::PASSED => ItemState::Passed,
                        settled::NEEDS_LOOK => ItemState::NeedsLook,
                        _ => ItemState::Pending,
                    });
                    if state == settled::INTERRUPTED && reason.as_deref() == Some(RECONCILE) {
                        r.reconciled = true;
                    }
                }
                EventPayload::AttemptStarted {
                    node: n,
                    item: Some(item),
                    ..
                } if n == node => {
                    f.items.entry(item.clone()).or_default().attempts += 1;
                }
                EventPayload::NodeFinished {
                    node: n, status, ..
                } if n == node => {
                    f.finished = true;
                    f.finished_status = Some(status.clone());
                    // An abort leaves its running items open: pending once
                    // the node is over.
                    for r in f.items.values_mut() {
                        if r.state == Some(ItemState::Running) {
                            r.state = Some(ItemState::Pending);
                        }
                    }
                }
                _ => {}
            }
        }
        f
    }

    /// Whether the visit is still under way (no `node_finished` after it).
    pub fn is_open(&self) -> bool {
        !self.finished
    }

    /// The record of `id`.
    pub fn item(&self, id: &str) -> Option<&ItemRecord> {
        self.items.get(id)
    }

    /// Of `ids` (the visit's items in order), those without a terminal
    /// state: what a resume, a supervisor retry and `rerun: not_passed` run
    /// again.
    pub fn remaining<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        ids.into_iter()
            .filter(|id| !self.items.get(*id).is_some_and(ItemRecord::is_terminal))
            .collect()
    }

    /// The visit's counts. `total` is the visit's item count; an item never
    /// admitted is pending.
    pub fn counts(&self) -> ListCounts {
        let mut c = ListCounts {
            total: self.total,
            ..ListCounts::default()
        };
        for r in self.items.values() {
            match r.state {
                Some(ItemState::Passed) => {
                    c.passed += 1;
                    if r.passed_with_note() {
                        c.passed_with_note += 1;
                    }
                }
                Some(ItemState::NeedsLook) => c.needs_look += 1,
                Some(ItemState::Running) => c.running += 1,
                _ => {}
            }
        }
        c.processed = c.passed + c.needs_look;
        c.pending = c.total.saturating_sub(c.processed + c.running);
        c
    }
}

/// The one-line progress label of a list: "12 of 43 processed, 11 passed, 1
/// needs a look".
pub fn progress_label(c: &ListCounts) -> String {
    let look = match c.needs_look {
        1 => "1 needs a look".to_string(),
        n => format!("{n} need a look"),
    };
    format!(
        "{} of {} processed, {} passed, {look}",
        c.processed, c.total, c.passed
    )
}

/// A `list` goal criterion read from the journal: whether `node`'s latest
/// visit processed every item with none needing a look, and the line that
/// says where it stands ("list `x`: 12 of 43 processed, ..."). The goal
/// check at the finish node and the run surfaces before it share it.
pub fn list_criterion(events: &[Event], node: &str) -> (bool, String) {
    let Some(fold) = ListFold::of(events, node) else {
        return (false, format!("list `{node}` never ran"));
    };
    let c = fold.counts();
    (
        c.processed == c.total && c.needs_look == 0,
        format!("list `{node}`: {}", progress_label(&c)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn started(seq: u64, total: u32, reopened: bool) -> Event {
        ev(
            seq,
            EventPayload::ListStarted {
                node: "l".into(),
                total,
                filtered_out: 0,
                items_digest: "d".into(),
                batch_size: 2,
                parallel: 1,
                reopened,
            },
        )
    }

    fn settled(seq: u64, item: &str, state: &str, verdict: Option<&str>, round: u32) -> Event {
        ev(
            seq,
            EventPayload::ListItemSettled {
                node: "l".into(),
                item: item.into(),
                state: state.into(),
                verdict: verdict.map(str::to_string),
                note: None,
                reason: None,
                output: None,
                round,
                carried: false,
                check: None,
                digest: None,
            },
        )
    }

    fn batch(seq: u64, n: u32, items: &[&str]) -> Event {
        ev(
            seq,
            EventPayload::ListBatchStarted {
                node: "l".into(),
                batch: n,
                items: items.iter().map(|s| s.to_string()).collect(),
            },
        )
    }

    #[test]
    fn a_visit_counts_its_items_and_names_what_remains() {
        let events = vec![
            started(1, 4, false),
            batch(2, 1, &["a", "b"]),
            settled(3, "a", settled::PASSED, Some("accept_with_note"), 0),
            settled(4, "b", settled::REJECTED, Some("reject"), 1),
            batch(5, 2, &["c", "b"]),
            settled(6, "c", settled::NEEDS_LOOK, None, 0),
        ];
        let f = ListFold::of(&events, "l").unwrap();
        let c = f.counts();
        assert_eq!(
            (
                c.total,
                c.processed,
                c.passed,
                c.passed_with_note,
                c.needs_look,
                c.running,
                c.pending
            ),
            (4, 2, 1, 1, 1, 1, 1)
        );
        assert_eq!(f.remaining(["a", "b", "c", "d"]), ["b", "d"]);
        assert!(f.is_open());
        assert_eq!(f.item("b").unwrap().round, 1);
        assert_eq!(
            progress_label(&c),
            "2 of 4 processed, 1 passed, 1 needs a look"
        );
    }

    /// A passed item's record carries its final note, not a note of the
    /// round its check rejected.
    #[test]
    fn a_passed_item_shows_only_its_final_note() {
        let mut rejected = settled(3, "a", settled::REJECTED, Some("reject"), 1);
        if let EventPayload::ListItemSettled { note, .. } = &mut rejected.payload {
            *note = Some("too vague".into());
        }
        let events = vec![
            started(1, 1, false),
            batch(2, 1, &["a"]),
            rejected,
            batch(4, 2, &["a"]),
            settled(5, "a", settled::PASSED, Some("accept"), 1),
        ];
        let f = ListFold::of(&events, "l").unwrap();
        assert_eq!(f.item("a").unwrap().note, None);
    }

    /// A check rejection followed by a failed retry keeps the rejection.
    #[test]
    fn a_rejection_survives_a_failed_retry() {
        let mut failed = settled(5, "a", settled::NEEDS_LOOK, None, 1);
        if let EventPayload::ListItemSettled { reason, .. } = &mut failed.payload {
            *reason = Some("failed".into());
        }
        let events = vec![
            started(1, 1, false),
            batch(2, 1, &["a"]),
            settled(3, "a", settled::REJECTED, Some("reject"), 1),
            batch(4, 2, &["a"]),
            failed,
        ];
        let f = ListFold::of(&events, "l").unwrap();
        let a = f.item("a").unwrap();
        assert_eq!(a.reason.as_deref(), Some("failed"));
        assert!(a.check_rejected);
        let list = crate::list::RunList::of(&f, None);
        let outcomes = crate::run_outcome::lists::list_outcomes(None, &[list]);
        assert_eq!(outcomes[0].check_failed, 1);
    }

    #[test]
    fn a_reopen_continues_the_visit_and_a_new_visit_starts_afresh() {
        let done = |seq| {
            ev(
                seq,
                EventPayload::NodeFinished {
                    node: "l".into(),
                    status: "failed".into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
                },
            )
        };
        let mut events = vec![
            started(1, 2, false),
            settled(2, "a", settled::PASSED, Some("accept"), 0),
            settled(3, "b", settled::NEEDS_LOOK, None, 0),
            done(4),
        ];
        assert!(!ListFold::of(&events, "l").unwrap().is_open());
        events.push(started(5, 2, true));
        events.push(settled(6, "b", settled::INTERRUPTED, None, 0));
        let f = ListFold::of(&events, "l").unwrap();
        assert!(f.is_open(), "a reopen is open again");
        assert_eq!(f.remaining(["a", "b"]), ["b"], "the passed item is kept");
        events.push(started(7, 2, false));
        let fresh = ListFold::of(&events, "l").unwrap();
        assert_eq!(fresh.remaining(["a", "b"]), ["a", "b"]);
        let prev = ListFold::previous(&events, "l").unwrap();
        assert_eq!(prev.item("a").unwrap().state, Some(ItemState::Passed));
    }
}
