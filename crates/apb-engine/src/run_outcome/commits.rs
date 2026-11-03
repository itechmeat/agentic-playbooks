//! The commits a run's nodes made (C7), and what their `Apb-Run` trailers
//! say: a commit that names this run links back to it, one without the
//! trailer or naming another run does not. apb never writes or rewrites the
//! trailer; it only reports it.

use serde::Serialize;

use crate::event::{AttemptHead, CommittedArtifact, Event, EventPayload};

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
    /// For each listed commit, in the same order, the attempt that made it
    /// when the heads its attempts left tell; empty when no head was
    /// recorded (an older event, a node without attempts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<Vec<Option<CommitAttempt>>>", optional)
    )]
    pub by_attempt: Vec<Option<CommitAttempt>>,
}

/// The attempt that made a commit.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CommitAttempt {
    /// The attempt id (the seq of its `attempt_started`).
    pub id: u64,
    /// The attempt's number, as its `attempt_started` counts it.
    pub number: u32,
}

/// Which attempt made each listed commit (newest first): a commit belongs
/// to the earliest attempt whose recorded head it is not past. The commits
/// are walked from the newest; meeting an earlier attempt's head moves the
/// attribution to that attempt. Display only: a merge or a rewrite inside
/// the range can make it inexact.
fn attribute(
    events: &[Event],
    commits: &[CommittedArtifact],
    heads: &[AttemptHead],
) -> Vec<Option<CommitAttempt>> {
    if heads.is_empty() {
        return Vec::new();
    }
    let number = |id: u64| {
        events.iter().find_map(|e| match &e.payload {
            EventPayload::AttemptStarted { attempt, .. } if e.seq == id => Some(*attempt),
            _ => None,
        })
    };
    let mut idx = heads.len() - 1;
    commits
        .iter()
        .map(|c| {
            while idx > 0 && c.sha == heads[idx - 1].head {
                idx -= 1;
            }
            let id = heads[idx].attempt;
            number(id).map(|number| CommitAttempt { id, number })
        })
        .collect()
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
                attempts,
                ..
            } => Some(NodeCommits {
                node: node.clone(),
                before: before.clone(),
                after: after.clone(),
                commits: commits.clone(),
                omitted: *omitted,
                by_attempt: attribute(events, commits, attempts),
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
        for (i, a) in c.commits.iter().enumerate() {
            let short: String = a.sha.chars().take(12).collect();
            let tag = TrailerState::of(a, run_id).tag();
            let by = match c.by_attempt.get(i).copied().flatten() {
                Some(at) => format!(" (committed by attempt {})", at.number),
                None => String::new(),
            };
            out.push(format!("{}: {short} {}{tag}{by}", c.node, a.subject));
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
                branch: None,
                attempt: None,
                attempts: Vec::new(),
            },
        }
    }

    /// Each commit is attributed to the attempt whose recorded head range
    /// holds it, by the attempt's own number.
    #[test]
    fn a_commit_names_the_attempt_that_made_it() {
        let started = |seq: u64, attempt: u32| Event {
            seq,
            ts: 0,
            payload: EventPayload::AttemptStarted {
                item: None,
                node: "impl".into(),
                attempt,
                agent: "a".into(),
                soul_delivery: None,
                skills_mode: None,
                pid: None,
                spawn_ms: None,
                model: None,
                workdir: None,
                transcript: None,
                token: None,
                isolation: None,
                exec: None,
            },
        };
        let mut done = committed(
            9,
            "impl",
            vec![
                commit("c3", "third", None),
                commit("c2", "second", None),
                commit("c1", "first", None),
            ],
            0,
        );
        if let EventPayload::ArtifactsCommitted {
            attempts, attempt, ..
        } = &mut done.payload
        {
            *attempts = vec![
                AttemptHead {
                    attempt: 2,
                    head: "c1".into(),
                },
                AttemptHead {
                    attempt: 5,
                    head: "c1".into(),
                },
                AttemptHead {
                    attempt: 7,
                    head: "c3".into(),
                },
            ];
            *attempt = Some(7);
        }
        let events = [started(2, 1), started(5, 2), started(7, 3), done];
        let commits = commits_from_events(&events);
        let by: Vec<Option<u32>> = commits[0]
            .by_attempt
            .iter()
            .map(|a| a.map(|a| a.number))
            .collect();
        // c3 and c2 after attempt 2 left c1: attempt 3; c1 by attempt 1
        // (attempt 2 committed nothing).
        assert_eq!(by, [Some(3), Some(3), Some(1)]);
        assert_eq!(
            commit_lines(&commits, "r1")[2],
            "impl: c1 first (committed by attempt 1)"
        );
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
