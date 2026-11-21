//! The list nodes of a run as its outcome tells them: one [`ListOutcome`]
//! per list node with its counts and how its items were checked, the
//! evidence an item check gives, the items that need a person's look and
//! the lists whose items passed on the agent's own report.

use apb_core::list::verdict::{ItemState, NeedsLookReason};
use apb_core::list::{CheckSource, ListSpec};
use apb_core::schema::Playbook;
use serde::Serialize;

use super::claims::{Evidence, EvidenceBasis, EvidenceSource, Gap, GapKind};
use crate::list::{RunList, RunListItem};

/// The most item gaps an outcome lists; the rest is one "and N more" line.
pub const ITEM_GAP_CAP: usize = 30;

/// One list node's visit, counted.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// What decided each item's check (`CheckSource::kind`); absent
    /// without a check.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub check_kind: Option<&'static str>,
    /// The visit's pilot, when the list runs one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pilot: Option<crate::list::view::PilotView>,
    /// The visit ended early: the sentence its `list_stopped` carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub stopped: Option<String>,
    /// The list's skip, when it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub skip: Option<crate::list::skip_record::SkipView>,
    /// The items a skip settled in this run: inherited passes, never
    /// checked here, so the check evidence leaves them out.
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    pub skipped: u32,
}

impl ListOutcome {
    /// `List triage: 43 processed, 41 passed (2 with a note), 2 need a look`.
    pub fn line(&self) -> String {
        let mut line = format!("List {}: {} processed", self.node, self.processed);
        if let Some(s) = self.skip.as_ref().filter(|s| s.skipped > 0) {
            line.push_str(&match s.source.as_deref() {
                Some(run) => format!(
                    ", {} skipped (passed in run {run}, not checked again)",
                    s.skipped
                ),
                None => format!(
                    ", {} skipped (passed in earlier runs, not checked again)",
                    s.skipped
                ),
            });
        }
        if let Some(s) = self.skip.as_ref().filter(|s| s.not_inherited > 0) {
            line.push_str(&format!(
                ", {} not skipped (the output they passed with could not be carried)",
                s.not_inherited
            ));
        }
        if let Some(s) = self.skip.as_ref().filter(|s| s.reworked > 0) {
            line.push_str(&format!(", {}", crate::list::skip_taken_back(s.reworked)));
        }
        line.push_str(&format!(", {} passed", self.passed));
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
        if let Some(p) = &self.pilot
            && p.state == "waiting"
        {
            line.push_str(&format!("; pilot round {} waits for a decision", p.round));
        }
        if let Some(detail) = &self.stopped {
            line.push_str(&format!("; stopped early: {detail}"));
        }
        if let Some(why) = self
            .skip
            .as_ref()
            .and_then(|s| s.stopped_stamping.as_deref())
        {
            line.push_str(&format!("; stamping stopped: {why}"));
        }
        line
    }

    /// The engine's item check as evidence, once every item was
    /// processed (a list under way is told by its line), over the items
    /// checked in this run only: a skipped item's pass is inherited, told
    /// apart by the line with its source run, never executed evidence here.
    /// `None` for a list without a check and for one whose every item was
    /// skipped.
    fn evidence(&self) -> Option<Evidence> {
        let check = self.check.as_deref()?;
        let checked = self.processed.saturating_sub(self.skipped);
        let passed = self.passed.saturating_sub(self.skipped);
        (checked > 0 && self.processed == self.total).then(|| Evidence {
            source: EvidenceSource::ItemCheck,
            subject: self.node.clone(),
            result: if self.check_failed == 0 {
                "passed"
            } else {
                "failed"
            }
            .into(),
            detail: Some(if self.skipped == 0 {
                format!("{passed} of {checked} items passed, {check}")
            } else {
                format!("{passed} of {checked} items checked in this run passed, {check}")
            }),
            basis: match self.check_kind {
                Some("script") => EvidenceBasis::Executed,
                Some("connector") => EvidenceBasis::Read,
                Some("judge") => EvidenceBasis::Model,
                _ => EvidenceBasis::AgentText,
            },
            freshness: None,
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

fn spec_of<'a>(playbook: Option<&'a Playbook>, node: &str) -> Option<&'a ListSpec> {
    playbook.and_then(|p| p.node(node)).and_then(|n| n.list())
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
            check: spec_of(playbook, &l.node).and_then(check_words),
            check_kind: spec_of(playbook, &l.node)
                .and_then(|s| s.check.as_ref()?.source())
                .map(|c| c.kind()),
            pilot: l.pilot.clone(),
            stopped: l.stopped.clone(),
            skip: l.skip.clone(),
            skipped: l.items.iter().filter(|i| i.skipped_from.is_some()).count() as u32,
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
            unresolved_send: false,
            check_rejected,
            skipped_from: None,
        }
    }

    #[test]
    fn the_line_names_skipped_items() {
        let mut o = ListOutcome {
            node: "work".into(),
            total: 5,
            processed: 5,
            passed: 5,
            passed_with_note: 0,
            needs_look: 0,
            check_failed: 0,
            check: None,
            check_kind: None,
            pilot: None,
            stopped: None,
            skip: Some(crate::list::skip_record::SkipView {
                skipped: 3,
                read_items: 4,
                source: Some("r0".into()),
                truncated: false,
                refused: None,
                not_inherited: 0,
                stopped_stamping: None,
                reworked: 0,
            }),
            skipped: 3,
        };
        assert_eq!(
            o.line(),
            "List work: 5 processed, 3 skipped (passed in run r0, not checked again), 5 passed, 0 need a look"
        );
        o.skip.as_mut().unwrap().skipped = 0;
        assert!(!o.line().contains("skipped"), "{}", o.line());
    }

    /// A skipped item's pass is inherited: the check evidence counts only
    /// the items checked in this run, and a list whose every item was
    /// skipped gives no check evidence at all.
    #[test]
    fn skipped_items_are_never_executed_check_evidence() {
        let mut o = ListOutcome {
            node: "work".into(),
            total: 3,
            processed: 3,
            passed: 3,
            passed_with_note: 0,
            needs_look: 0,
            check_failed: 0,
            check: Some("script scripts/c.sh".into()),
            check_kind: Some("script"),
            pilot: None,
            stopped: None,
            skip: None,
            skipped: 1,
        };
        let e = o.evidence().expect("two items were checked here");
        assert_eq!(
            e.detail.as_deref(),
            Some("2 of 2 items checked in this run passed, script scripts/c.sh")
        );
        o.skipped = 3;
        assert!(
            o.evidence().is_none(),
            "an all-skipped list checked nothing here"
        );
    }

    #[test]
    fn the_line_names_a_waiting_pilot_and_an_early_stop() {
        use crate::list::view::PilotView;
        let mut o = ListOutcome {
            node: "work".into(),
            total: 5,
            processed: 3,
            passed: 3,
            passed_with_note: 0,
            needs_look: 0,
            check_failed: 0,
            check: None,
            check_kind: None,
            pilot: Some(PilotView {
                round: 1,
                items: Vec::new(),
                passed: 3,
                needs_look: 0,
                rest: 2,
                state: "waiting".into(),
            }),
            stopped: None,
            skip: None,
            skipped: 0,
        };
        assert!(
            o.line().contains("; pilot round 1 waits for a decision"),
            "{}",
            o.line()
        );
        // A pilot that is not waiting names nothing.
        o.pilot.as_mut().unwrap().state = "continued".into();
        assert!(!o.line().contains("pilot"), "{}", o.line());
        o.pilot = None;
        o.stopped = Some("2 of 2 settled items need a look".into());
        assert!(
            o.line()
                .contains("; stopped early: 2 of 2 settled items need a look"),
            "{}",
            o.line()
        );
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
