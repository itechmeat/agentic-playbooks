//! The commits a run's nodes made (C7), and what their `Apb-Run` trailers
//! say: a commit that names this run links back to it, one without the
//! trailer or naming another run does not. apb never writes or rewrites the
//! trailer; it only reports it.

use serde::Serialize;

use crate::event::{CommittedArtifact, Event, EventPayload};

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

/// What a commit's `Apb-Run` trailer says about the run that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrailerState<'a> {
    /// The trailers were not read (an old event, or an old git).
    NotRead,
    /// The commit names this run.
    ThisRun,
    /// The commit has no `Apb-Run` trailer.
    Missing,
    /// The commit names other runs only.
    Other(&'a [String]),
}

impl<'a> TrailerState<'a> {
    /// The state of `commit` for the run `run_id`.
    pub fn of(commit: &'a CommittedArtifact, run_id: &str) -> Self {
        match commit.apb_run.as_deref() {
            None => TrailerState::NotRead,
            Some([]) => TrailerState::Missing,
            Some(ids) if ids.iter().any(|id| id == run_id) => TrailerState::ThisRun,
            Some(ids) => TrailerState::Other(ids),
        }
    }

    /// The tag a commit line carries, empty when there is nothing to flag.
    pub fn tag(&self) -> String {
        match self {
            TrailerState::NotRead | TrailerState::ThisRun => String::new(),
            TrailerState::Missing => " [no Apb-Run trailer]".to_string(),
            TrailerState::Other(ids) => format!(" [Apb-Run names {}]", ids.join(", ")),
        }
    }
}

/// `node: abc1234 subject` lines for a terminal, one per commit, flagged
/// when its trailer does not name `run_id`.
pub fn commit_lines(commits: &[NodeCommits], run_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in commits {
        for a in &c.commits {
            let short: String = a.sha.chars().take(12).collect();
            let tag = TrailerState::of(a, run_id).tag();
            out.push(format!("{}: {short} {}{tag}", c.node, a.subject));
        }
        if c.omitted > 0 {
            out.push(format!("{}: and {} more", c.node, c.omitted));
        }
    }
    out
}

/// The done line of one node's commits: `impl: 3 commits (1 without an
/// Apb-Run trailer)`.
pub fn done_line(c: &NodeCommits, run_id: &str) -> String {
    let n = c.commits.len() + c.omitted;
    let states: Vec<TrailerState<'_>> = c
        .commits
        .iter()
        .map(|a| TrailerState::of(a, run_id))
        .collect();
    let missing = states
        .iter()
        .filter(|s| **s == TrailerState::Missing)
        .count();
    let other = states
        .iter()
        .filter(|s| matches!(s, TrailerState::Other(_)))
        .count();
    let mut flags = Vec::new();
    if missing > 0 {
        flags.push(format!("{missing} without an Apb-Run trailer"));
    }
    if other > 0 {
        flags.push(format!("{other} naming another run"));
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!(" ({})", flags.join(", "))
    };
    let word = if n == 1 { "commit" } else { "commits" };
    format!("{}: {n} {word}{flags}", c.node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, subject: &str, apb_run: Option<&[&str]>) -> CommittedArtifact {
        CommittedArtifact {
            sha: sha.into(),
            subject: subject.into(),
            apb_run: apb_run.map(|ids| ids.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn committed(seq: u64, node: &str, commits: Vec<CommittedArtifact>, omitted: usize) -> Event {
        Event {
            seq,
            ts: 0,
            payload: EventPayload::ArtifactsCommitted {
                node: node.into(),
                before: "b".into(),
                after: commits.first().map(|c| c.sha.clone()).unwrap_or_default(),
                commits,
                omitted,
            },
        }
    }

    #[test]
    fn commits_are_read_in_journal_order_and_flagged_by_their_trailer() {
        let events = [
            committed(
                1,
                "impl",
                vec![
                    commit("0123456789abcdef", "ours", Some(&["r1"])),
                    commit("1111111111111111", "bare", Some(&[])),
                    commit("2222222222222222", "theirs", Some(&["r0"])),
                ],
                0,
            ),
            committed(2, "docs", vec![commit("fedcba9876543210", "old", None)], 2),
        ];
        let commits = commits_from_events(&events);
        assert_eq!(commits.len(), 2);
        assert_eq!(
            commit_lines(&commits, "r1"),
            [
                "impl: 0123456789ab ours",
                "impl: 111111111111 bare [no Apb-Run trailer]",
                "impl: 222222222222 theirs [Apb-Run names r0]",
                "docs: fedcba987654 old",
                "docs: and 2 more",
            ]
        );
        assert_eq!(
            done_line(&commits[0], "r1"),
            "impl: 3 commits (1 without an Apb-Run trailer, 1 naming another run)"
        );
        // An old event read no trailers: nothing is flagged.
        assert_eq!(done_line(&commits[1], "r1"), "docs: 3 commits");
        // The wire shape the surfaces serialize: no `apb_run` when not read.
        assert_eq!(
            serde_json::to_string(&commits[1]).unwrap(),
            r#"{"node":"docs","before":"b","after":"fedcba9876543210","commits":[{"sha":"fedcba9876543210","subject":"old"}],"omitted":2}"#
        );
        assert!(
            serde_json::to_string(&commits[0].commits[1])
                .unwrap()
                .contains(r#""apb_run":[]"#)
        );
    }
}
