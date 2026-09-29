//! What a run produced beyond its node outputs, for the read-only run
//! surfaces (`run_report`, `run_status`, `apb runs <id>`, the dashboard run
//! page): its goal criteria and their results (C1) and the commits its
//! nodes made on a git tree (C7). Every block is
//! absent when the run journaled nothing for it, so a run without them reads
//! exactly as before.

use std::path::Path;

use serde::Serialize;

use crate::event::{CommittedArtifact, Event, EventPayload};
use crate::run_view::RunView;

/// The commits one node execution made (one `artifacts_committed`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeCommits {
    pub node: String,
    /// `HEAD` before the node ran.
    pub before: String,
    /// `HEAD` after it.
    pub after: String,
    /// Newest first, at most 50.
    pub commits: Vec<CommittedArtifact>,
    /// Commits past the listed ones.
    pub omitted: usize,
}

/// Every `artifacts_committed` of `events`, in journal order.
pub fn commits_from_events(events: &[Event]) -> Vec<NodeCommits> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ArtifactsCommitted {
                node,
                before,
                after,
                commits,
                omitted,
            } => Some(NodeCommits {
                node: node.clone(),
                before: before.clone(),
                after: after.clone(),
                commits: commits.clone(),
                omitted: *omitted,
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
}

// --- goal criteria (C1) -------------------------------------------------------

/// One goal criterion and what the run found.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GoalCriterionResult {
    pub index: usize,
    pub description: String,
    /// `script`, `marker` or `manual`.
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
            };
            // The last result for this criterion (a resumed run may check
            // twice).
            let found = checked.iter().rev().find(|(idx, _, _)| *idx == i);
            GoalCriterionResult {
                index: i,
                description: c.description.clone(),
                check: check.to_string(),
                status: found.map_or(fallback, |(_, s, _)| s).to_string(),
                detail: found.and_then(|(_, _, d)| (*d).clone()),
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

/// `abc1234 subject` lines for a terminal: one per commit, the node first.
pub fn commit_lines(commits: &[NodeCommits]) -> Vec<String> {
    let mut out = Vec::new();
    for c in commits {
        for a in &c.commits {
            let short: String = a.sha.chars().take(12).collect();
            out.push(format!("{}: {short} {}", c.node, a.subject));
        }
        if c.omitted > 0 {
            out.push(format!("{}: and {} more", c.node, c.omitted));
        }
    }
    out
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
    fn commits_are_read_in_journal_order_and_printed_per_node() {
        let ev = |seq: u64, node: &str, sha: &str, omitted: usize| Event {
            seq,
            ts: 0,
            payload: EventPayload::ArtifactsCommitted {
                node: node.into(),
                before: "b".into(),
                after: sha.into(),
                commits: vec![CommittedArtifact {
                    sha: sha.into(),
                    subject: format!("change {node}"),
                }],
                omitted,
            },
        };
        let events = [
            ev(1, "impl", "0123456789abcdef", 0),
            ev(2, "docs", "fedcba9876543210", 2),
        ];
        let commits = commits_from_events(&events);
        assert_eq!(commits.len(), 2);
        assert_eq!(
            commit_lines(&commits),
            [
                "impl: 0123456789ab change impl",
                "docs: fedcba987654 change docs",
                "docs: and 2 more",
            ]
        );
        // The wire shape the surfaces serialize.
        assert_eq!(
            serde_json::to_string(&commits[0]).unwrap(),
            r#"{"node":"impl","before":"b","after":"0123456789abcdef","commits":[{"sha":"0123456789abcdef","subject":"change impl"}],"omitted":0}"#
        );
    }
}
