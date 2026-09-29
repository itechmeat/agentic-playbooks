//! What a run produced beyond its node outputs, for the read-only run
//! surfaces (`run_report`, `run_status`, `apb runs <id>`, the dashboard run
//! page): the commits its nodes made on a git tree (C7). Every block is
//! absent when the run journaled nothing for it, so a run without them reads
//! exactly as before.

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
