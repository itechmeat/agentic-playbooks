//! The list nodes of a run as its outcome tells them: one [`ListOutcome`]
//! per list node with its counts and how its items were checked, the
//! evidence an item check gives, the items that need a person's look and
//! the lists whose items passed on the agent's own report.

use apb_core::list::verdict::{ItemState, NeedsLookReason};
use apb_core::list::{CheckSource, ListSpec};
use apb_core::schema::Playbook;
use serde::Serialize;

use super::claims::{Evidence, EvidenceSource, Gap, GapKind};
use crate::list::{RunList, RunListItem};

/// The most item gaps an outcome lists; the rest is one "and N more" line.
pub const ITEM_GAP_CAP: usize = 30;

/// One list node's visit, counted.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListOutcome {
    pub node: String,
    pub total: u32,
    pub processed: u32,
    pub passed: u32,
    pub passed_with_note: u32,
    pub needs_look: u32,
    /// The items that need a look because their check rejected them or
    /// could not tell: what the check evidence reports as failed.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub check_failed: u32,
    /// How each item was checked (`connector asana.get_task`); absent when
    /// the items passed on the agent's own report.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub check: Option<String>,
}

impl ListOutcome {
    /// `List triage: 43 processed, 41 passed (2 with a note), 2 need a look`.
    pub fn line(&self) -> String {
        let mut line = format!(
            "List {}: {} processed, {} passed",
            self.node, self.processed, self.passed
        );
        if self.passed_with_note > 0 {
            line.push_str(&format!(" ({} with a note)", self.passed_with_note));
        }
        line.push_str(&match self.needs_look {
            1 => ", 1 needs a look".to_string(),
            n => format!(", {n} need a look"),
        });
        let open = self.total.saturating_sub(self.processed);
        if open > 0 {
            line.push_str(&format!(", {open} of {} not processed yet", self.total));
        }
        line
    }

    /// The engine's item check as evidence, once every item was
    /// processed (a list under way is told by its line); `None` for a list
    /// without a check.
    fn evidence(&self) -> Option<Evidence> {
        let check = self.check.as_deref()?;
        (self.processed > 0 && self.processed == self.total).then(|| Evidence {
            source: EvidenceSource::ItemCheck,
            subject: self.node.clone(),
            result: if self.check_failed == 0 {
                "passed"
            } else {
                "failed"
            }
            .into(),
            detail: Some(format!(
                "{} of {} items passed, {check}",
                self.passed, self.processed
            )),
        })
    }
}

/// How `spec`'s check reads an item, in words.
fn check_words(spec: &ListSpec) -> Option<String> {
    Some(match spec.check.as_ref()?.source()? {
        CheckSource::Output(o) => format!("output {} equals {}", o.field, o.equals),
        CheckSource::Script(path) => format!("script {path}"),
        CheckSource::Connector(c) => format!("connector {}.{}", c.name, c.function),
        CheckSource::Judge(_) => "judge".to_string(),
    })
}

/// Whether the item's check put it in need of a look: it could not tell or
/// rejected it, even when a later step failure became the final reason.
fn item_check_failed(item: &RunListItem) -> bool {
    let by_reason = item
        .reason
        .as_deref()
        .and_then(NeedsLookReason::parse)
        .is_some_and(NeedsLookReason::from_check);
    by_reason || (item.check_rejected && item.state == ItemState::NeedsLook.as_str())
}

/// One [`ListOutcome`] per list in `lists`, the check read from the run's
/// playbook snapshot.
pub fn list_outcomes(playbook: Option<&Playbook>, lists: &[RunList]) -> Vec<ListOutcome> {
    lists
        .iter()
        .map(|l| ListOutcome {
            node: l.node.clone(),
            total: l.total,
            processed: l.processed,
            passed: l.passed,
            passed_with_note: l.passed_with_note,
            needs_look: l.needs_look,
            check_failed: l.items.iter().filter(|i| item_check_failed(i)).count() as u32,
            check: playbook
                .and_then(|p| p.node(&l.node))
                .and_then(|n| n.list())
                .and_then(check_words),
        })
        .collect()
}

/// The item checks the engine ran, one evidence line per list.
pub fn list_evidence(outcomes: &[ListOutcome]) -> Vec<Evidence> {
    outcomes.iter().filter_map(ListOutcome::evidence).collect()
}

/// Why an item needs a look, in words.
fn reason_words(item: &RunListItem) -> String {
    let reason = match item.reason.as_deref() {
        Some(word) => NeedsLookReason::parse(word).map_or(word, |r| match r {
            NeedsLookReason::Rejected => "its check rejected it and its re-runs are spent",
            NeedsLookReason::CheckUnknown => "its check could not tell",
            NeedsLookReason::UnknownSend => "a send it made has an unknown outcome",
            NeedsLookReason::Failed => "its step failed",
            NeedsLookReason::ItemTooLarge => "the item is too large to hand to the step",
        }),
        None => "needs a look",
    };
    match item.note.as_deref().filter(|n| !n.is_empty()) {
        Some(note) => format!("{reason}: {note}"),
        None => reason.to_string(),
    }
}

/// The lists whose items passed with no check, then every item that needs a
/// look (at most [`ITEM_GAP_CAP`] of them, then one "and N more" line).
pub fn list_gaps(outcomes: &[ListOutcome], lists: &[RunList]) -> Vec<Gap> {
    let mut out: Vec<Gap> = outcomes
        .iter()
        .filter(|o| o.check.is_none() && o.passed > 0)
        .map(|o| Gap {
            kind: GapKind::SelfReportedItems,
            subject: o.node.clone(),
            detail: Some(format!(
                "{} {} passed on the agent's own report, with no item check",
                o.passed,
                if o.passed == 1 { "item" } else { "items" }
            )),
        })
        .collect();
    let looks: Vec<(&str, &RunListItem)> = lists
        .iter()
        .flat_map(|l| {
            l.items
                .iter()
                .filter(|i| i.state == ItemState::NeedsLook.as_str())
                .map(move |i| (l.node.as_str(), i))
        })
        .collect();
    out.extend(looks.iter().take(ITEM_GAP_CAP).map(|(node, i)| Gap {
        kind: GapKind::ItemNeedsLook,
        subject: format!("{node}/{}", i.id),
        detail: Some(reason_words(i)),
    }));
    if looks.len() > ITEM_GAP_CAP {
        out.push(Gap {
            kind: GapKind::ItemNeedsLook,
            subject: format!("and {} more", looks.len() - ITEM_GAP_CAP),
            detail: Some("the run's lists name every item".into()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(state: &str, reason: Option<&str>, check_rejected: bool) -> RunListItem {
        RunListItem {
            id: "a".into(),
            state: state.into(),
            round: 1,
            verdict: None,
            note: None,
            reason: reason.map(str::to_string),
            attempts: 2,
            check_rejected,
        }
    }

    #[test]
    fn a_rejection_followed_by_a_failed_retry_still_counts_as_check_failed() {
        let rejected_then_failed = item("needs_look", Some("failed"), true);
        let failed_only = item("needs_look", Some("failed"), false);
        let retried_ok = item("passed", None, true);
        assert!(item_check_failed(&rejected_then_failed));
        assert!(!item_check_failed(&failed_only));
        assert!(!item_check_failed(&retried_ok));
        assert!(item_check_failed(&item(
            "needs_look",
            Some("check_unknown"),
            false
        )));
    }
}
