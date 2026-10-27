//! What a run produced beyond its node outputs, for the read-only run
//! surfaces (`run_report`, `run_status`, `apb runs <id>`, the dashboard run
//! page): its goal criteria and their results (C1), the commits its
//! nodes made on a git tree (C7), and the inbox gaps its connector reads
//! journaled. Every block is
//! absent when the run journaled nothing for it, so a run without them reads
//! exactly as before.
//!
//! Submodules: `claims` (evidence and gaps), `baseline` (the baseline check's view), `notes` (findings and
//! warnings), `calls` (connector calls by what they say about the outcome),
//! `commits` (the commits and their `Apb-Run` trailers), `intents` (the send
//! intents of the send ledger), `lists` (the list nodes' counts, item checks
//! and items that need a look) and `summary` (the human outcome every
//! surface shows, and the cheap "needs a look" marker of a run listing).

use std::path::Path;

pub mod baseline;
pub mod calls;
pub mod claims;
pub mod commits;
pub mod intents;
pub mod lists;
pub mod notes;
pub mod summary;

pub use baseline::{RunBaseline, baseline_from};
pub use commits::{NodeCommits, TrailerState, commit_lines, commits_from_events};
pub use intents::{RunIntent, intents_from, outcome_intents};
pub use lists::ListOutcome;
pub use notes::RunNotes;
pub use summary::{AGENT_SUMMARY_TITLE, NeedsLook, OutcomeSummary};

use serde::Serialize;

use crate::event::{Event, EventPayload};
use crate::run_view::RunView;

/// One inbox gap a run's connector read journaled (one `inbox_gap`): unread
/// events a connector inbox read found lost to inbox retention. A run with
/// any of these did not see its inbox whole.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunInboxGap {
    /// The node whose connector call made the read.
    pub node: String,
    pub connector: String,
    pub account: String,
    /// First lost sequence number (the consumer's cursor plus one).
    pub from: u64,
    /// Last lost sequence number, inclusive.
    pub through: u64,
}

/// Every `inbox_gap` of `events`, in journal order. The call path appends at
/// most one event per distinct gap, so no dedup happens here: the journal is
/// already one-per-gap.
pub fn inbox_gaps_from(events: &[Event]) -> Vec<RunInboxGap> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxGap {
                node_id,
                connector,
                account,
                from,
                through,
            } => Some(RunInboxGap {
                node: node_id.clone(),
                connector: connector.clone(),
                account: account.clone(),
                from: *from,
                through: *through,
            }),
            _ => None,
        })
        .collect()
}

impl RunView {
    /// The commits the run's nodes made, empty when none did (or the tree
    /// is not a git repository).
    pub fn commits(&self) -> Vec<NodeCommits> {
        commits_from_events(&self.events)
    }

    /// The inbox gaps the run's connector reads journaled, empty when none:
    /// retention dropped unread events, so the inbox was not seen whole.
    pub fn inbox_gaps(&self) -> Vec<RunInboxGap> {
        inbox_gaps_from(&self.events)
    }
}

impl RunView {
    /// The run's baseline check with what the journal recorded; `None`
    /// when the playbook declares none.
    pub fn baseline(&self, run_dir: &Path) -> Option<RunBaseline> {
        let playbook = crate::legacy_snapshot::load_run_playbook(run_dir);
        baseline_from(playbook.as_ref(), &self.events)
    }

    /// The findings and warnings the run's attempts reported.
    pub fn notes(&self) -> RunNotes {
        RunNotes::from_events(&self.events)
    }

    /// The human outcome of the run (see [`OutcomeSummary`]).
    pub fn outcome(&self, run_dir: &Path) -> OutcomeSummary {
        OutcomeSummary::build(self, run_dir)
    }

    /// The run's playbook snapshot and the blocks read against it, parsed
    /// once, for a surface that shows them next to the outcome.
    pub fn readout(&self, run_dir: &Path) -> RunReadout {
        self.readout_of(crate::legacy_snapshot::load_run_playbook(run_dir))
    }

    /// [`RunView::readout`] over a playbook snapshot the caller already
    /// parsed.
    pub fn readout_of(&self, playbook: Option<apb_core::schema::Playbook>) -> RunReadout {
        RunReadout {
            goal: goal_from(playbook.as_ref(), &self.events),
            lists: crate::list::run_lists(playbook.as_ref(), &self.events),
            baseline: baseline_from(playbook.as_ref(), &self.events),
            notes: self.notes(),
            playbook,
        }
    }
}

/// What [`RunView::readout`] read: one parse of the playbook snapshot, so
/// the goal, the baseline, the notes and the outcome a surface shows side
/// by side come from the same reading.
#[derive(Debug, Clone)]
pub struct RunReadout {
    pub playbook: Option<apb_core::schema::Playbook>,
    pub goal: Option<RunGoal>,
    pub baseline: Option<RunBaseline>,
    pub notes: RunNotes,
    /// Every list node that started a visit, with its items.
    pub lists: Vec<crate::list::RunList>,
}

// --- goal criteria (C1) -------------------------------------------------------

/// One goal criterion and what the run found.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalCriterionResult {
    pub index: usize,
    pub description: String,
    /// `script`, `marker`, `list` or `manual`.
    pub check: String,
    /// `passed`, `failed`, `error`, `manual` (a person confirms it), or
    /// `pending` while the run has not reached a finish node.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}

/// The run's goal: the statement, each criterion with its result, and the
/// counts. Absent for a playbook without a goal.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunGoal {
    pub statement: String,
    /// `goal.enforce: true`: a failed script or marker criterion fails the
    /// run.
    pub enforce: bool,
    /// Whether the criteria were checked (the run reached a finish node).
    pub checked: bool,
    pub criteria: Vec<GoalCriterionResult>,
    pub passed: usize,
    pub failed: usize,
    pub manual: usize,
}

/// The goal of the run's playbook snapshot with the results `events`
/// journaled; `None` when the playbook declares no goal. Criteria not yet
/// checked read `pending` (`manual` for manual ones).
pub fn goal_from(
    playbook: Option<&apb_core::schema::Playbook>,
    events: &[Event],
) -> Option<RunGoal> {
    use apb_core::schema::GoalCheck;
    let goal = playbook?.goal.as_ref()?;
    let checked: Vec<(usize, &str, &Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::GoalChecked {
                index,
                status,
                detail,
                ..
            } => Some((*index, status.as_str(), detail)),
            _ => None,
        })
        .collect();
    let criteria: Vec<GoalCriterionResult> = goal
        .criteria
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (check, fallback) = match &c.check {
                GoalCheck::Manual => ("manual", "manual"),
                GoalCheck::Marker { .. } => ("marker", "pending"),
                GoalCheck::Script { .. } => ("script", "pending"),
                GoalCheck::List { .. } => ("list", "pending"),
            };
            // The last result for this criterion (a resumed run may check
            // twice).
            let found = checked.iter().rev().find(|(idx, _, _)| *idx == i);
            // A list criterion not checked yet says where its list stands.
            let live = || match &c.check {
                GoalCheck::List { node } => Some(crate::list::list_criterion(events, node).1),
                _ => None,
            };
            GoalCriterionResult {
                index: i,
                description: c.description.clone(),
                check: check.to_string(),
                status: found.map_or(fallback, |(_, s, _)| s).to_string(),
                detail: match found {
                    Some((_, _, d)) => (*d).clone(),
                    None => live(),
                },
            }
        })
        .collect();
    let count = |s: &str| criteria.iter().filter(|c| c.status == s).count();
    Some(RunGoal {
        statement: goal.statement.clone(),
        enforce: goal.enforce,
        checked: !checked.is_empty(),
        passed: count("passed"),
        failed: count("failed") + count("error"),
        manual: count("manual"),
        criteria,
    })
}

impl RunView {
    /// The goal of the run at `run_dir` (read from its playbook snapshot)
    /// with the journaled results; `None` when the playbook has no goal.
    pub fn goal(&self, run_dir: &Path) -> Option<RunGoal> {
        let playbook = crate::legacy_snapshot::load_run_playbook(run_dir);
        goal_from(playbook.as_ref(), &self.events)
    }
}

impl RunGoal {
    /// The summary line: `2 passed, 1 failed, 1 manual (enforced)`, or
    /// `not checked yet` before the run reached a finish node.
    pub fn line(&self) -> String {
        // A goal with a statement only: nothing is ever checked, so "not
        // checked yet" would read as pending forever.
        if self.criteria.is_empty() {
            return "no criteria to check".to_string();
        }
        if !self.checked {
            return format!("not checked yet ({} criteria)", self.criteria.len());
        }
        let mut parts = vec![format!("{} passed", self.passed)];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.manual > 0 {
            parts.push(format!("{} manual", self.manual));
        }
        let mut line = parts.join(", ");
        if self.enforce {
            line.push_str(" (enforced)");
        }
        line
    }
}

// --- end of goal criteria -------------------------------------------------------

/// One plain line per inbox gap, for a terminal or a report: which inbox
/// lost which unread events, and which node's read found it.
pub fn inbox_gap_lines(gaps: &[RunInboxGap]) -> Vec<String> {
    gaps.iter()
        .map(|g| {
            // The consumer's cursor may predate this run, so the line says
            // the events were never read, not that this run missed them.
            let lost = if g.from == g.through {
                format!(
                    "unread event {} was dropped by inbox retention before it was read",
                    g.from
                )
            } else {
                format!(
                    "unread events {}-{} were dropped by inbox retention before they were read",
                    g.from, g.through
                )
            };
            format!(
                "{}/{}: {lost} (found by {})",
                g.connector, g.account, g.node
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A goal with a statement only keeps its statement and never reads as
    /// pending.
    #[test]
    fn a_statement_only_goal_reads_as_having_nothing_to_check() {
        let pb = apb_core::schema::Playbook::from_yaml(
            "schema: 2\nid: g\nname: g\nversion: 1.0.0\ngoal:\n  statement: Ship it\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        let g = goal_from(Some(&pb), &[]).unwrap();
        assert_eq!(g.statement, "Ship it");
        assert_eq!(g.line(), "no criteria to check");
    }

    #[test]
    fn the_goal_reads_pending_until_checked_then_the_journaled_results() {
        let pb = apb_core::schema::Playbook::from_yaml(
            r#"
schema: 2
id: g
name: g
version: 1.0.0
goal:
  statement: Ship the fix
  enforce: true
  criteria:
    - { description: tests pass, check: { type: script, path: scripts/t.sh } }
    - { description: says done, check: { type: marker, marker: DONE } }
    - { description: a person reads it }
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#,
        )
        .unwrap();
        assert!(goal_from(None, &[]).is_none());
        let before = goal_from(Some(&pb), &[]).unwrap();
        assert!(!before.checked);
        let statuses: Vec<&str> = before.criteria.iter().map(|c| c.status.as_str()).collect();
        assert_eq!(statuses, ["pending", "pending", "manual"]);
        assert_eq!(before.line(), "not checked yet (3 criteria)");
        let ev = |seq: u64, index: usize, status: &str, detail: Option<&str>| Event {
            seq,
            ts: 0,
            payload: EventPayload::GoalChecked {
                index,
                description: String::new(),
                check: String::new(),
                status: status.into(),
                detail: detail.map(str::to_string),
                enforced: index < 2,
            },
        };
        let events = [
            ev(1, 0, "passed", None),
            ev(2, 1, "failed", Some("marker `DONE` not found")),
            ev(3, 2, "manual", None),
        ];
        let after = goal_from(Some(&pb), &events).unwrap();
        assert!(after.checked);
        assert_eq!((after.passed, after.failed, after.manual), (1, 1, 1));
        assert_eq!(after.line(), "1 passed, 1 failed, 1 manual (enforced)");
        assert_eq!(
            after.criteria[1].detail.as_deref(),
            Some("marker `DONE` not found")
        );
    }

    #[test]
    fn inbox_gaps_are_read_in_journal_order_and_serialized_flat() {
        let ev = |seq: u64, node: &str, from: u64, through: u64| Event {
            seq,
            ts: 0,
            payload: EventPayload::InboxGap {
                node_id: node.into(),
                connector: "echo-hooks".into(),
                account: "main".into(),
                from,
                through,
            },
        };
        let events = [ev(1, "watch", 3, 6), ev(2, "watch", 1, 6)];
        let gaps = inbox_gaps_from(&events);
        assert_eq!(gaps.len(), 2, "one entry per journaled gap");
        assert_eq!(gaps[0].from, 3);
        assert_eq!(gaps[1].from, 1, "journal order, not sorted order");
        assert!(inbox_gaps_from(&[]).is_empty());
        assert_eq!(
            inbox_gap_lines(&gaps[..1]),
            [
                "echo-hooks/main: unread events 3-6 were dropped by inbox retention before they were read (found by watch)"
            ]
        );
        let one = RunInboxGap {
            from: 4,
            through: 4,
            ..gaps[0].clone()
        };
        assert_eq!(
            inbox_gap_lines(&[one]),
            [
                "echo-hooks/main: unread event 4 was dropped by inbox retention before it was read (found by watch)"
            ]
        );
        // The wire shape the surfaces serialize.
        assert_eq!(
            serde_json::to_string(&gaps[0]).unwrap(),
            r#"{"node":"watch","connector":"echo-hooks","account":"main","from":3,"through":6}"#
        );
    }
}
