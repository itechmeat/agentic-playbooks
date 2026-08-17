//! Channel for human_review decisions (`reviews.jsonl`). Mirrors control.rs:
//! decision makers (`apb review`, MCP review_decide, HTTP) append their
//! decision here, while drive only reads it and, based on it, writes a
//! ReviewDecided event. This does not violate the single-writer rule for
//! events: events.jsonl is still written only by drive.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommand {
    pub node: String,
    pub decision: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEntry {
    pub seq: u64,
    #[serde(flatten)]
    pub cmd: ReviewCommand,
}

/// Rejects a decision that names something other than a currently pending gate
/// of this run (issue #103.1).
///
/// The check lives here rather than in any one caller because `post_review` is
/// the single entry point of every decision surface (`apb review`, MCP
/// `review_decide`, `POST /api/runs/{id}/review`): before this, all three
/// happily wrote a decision for a node that does not exist, is not a gate, or
/// has no open request, returned a `posted_seq` that looks like success, and
/// left a record no drive would ever consume.
///
/// "Pending" is the same predicate every reporting surface already uses
/// (`progress::compute_with`): more `ReviewRequested` than `ReviewDecided`
/// events for the node. So a caller that was told a gate is pending can always
/// decide it, and nothing else is accepted. A pending gate whose decision is
/// already queued in the channel is no longer waiting for one, so the check
/// counts the channel too and refuses the duplicate.
///
/// Both reads are "cannot judge means accept". A run with no playbook snapshot
/// (pre-snapshot runs, and the bare run dirs the channel's own tests build) has
/// nothing to validate against, and a journal that will not read - most
/// plausibly a torn trailing line the drive is in the middle of appending -
/// cannot answer whether the gate is pending. `post_review` never read
/// `events.jsonl` at all before this check existed, so failing the write on
/// either would be a new way for a perfectly valid decision to be refused,
/// in exactly the live-run race this change set removes elsewhere. Rejecting a
/// decision is reserved for a journal that positively says the gate is not
/// waiting.
fn check_review_target(run_dir: &Path, node: &str) -> Result<(), EngineError> {
    use apb_core::schema::NodeKind;

    let Some(playbook) = crate::progress::load_run_playbook(run_dir) else {
        return Ok(());
    };
    let is_gate = playbook
        .node(node)
        .is_some_and(|n| matches!(n.kind, NodeKind::HumanReview { .. }));
    if !is_gate {
        return Err(EngineError::NotFound(format!(
            "node `{node}` is not a human_review node of this run's playbook `{}`",
            playbook.id
        )));
    }

    let Ok(events) = crate::event::read_all(run_dir) else {
        return Ok(());
    };
    let requested = crate::event::review_requested_count(&events, node);
    let decided = crate::event::review_decided_count(&events, node);
    if requested <= decided {
        return Err(EngineError::Conflict(format!(
            "node `{node}` has no review decision pending"
        )));
    }

    // The journal alone cannot see a decision that is already sitting in the
    // channel and has not been folded into a `ReviewDecided` event yet, so
    // requested-vs-decided would accept a second decision for the same open
    // request. The drive consumes the `decided`-th record for the node
    // (`scheduler.rs`, the HumanReview arm), which is exactly the count this
    // reads back: everything past that is queued and unconsumed. Once the
    // queue already answers every outstanding request, another decision is
    // not an answer, it is a stale extra a cyclic gate would consume on its
    // NEXT visit without anyone confirming it.
    let Ok(queued) = read_reviews_after(run_dir, None) else {
        return Ok(());
    };
    let unconsumed = queued
        .iter()
        .filter(|e| e.cmd.node == node)
        .count()
        .saturating_sub(decided);
    if unconsumed >= requested - decided {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for node `{node}`"
        )));
    }
    Ok(())
}

pub fn post_review(run_dir: &Path, cmd: ReviewCommand) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;
    check_review_target(run_dir, &cmd.node)?;

    let seq = read_reviews_after(run_dir, None)?.len() as u64;

    let entry = ReviewEntry { seq, cmd };
    let line = serde_json::to_string(&entry).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let path = run_dir.join("reviews.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    file.flush()?;

    Ok(seq)
}

pub fn read_reviews_after(
    run_dir: &Path,
    after_seq: Option<u64>,
) -> Result<Vec<ReviewEntry>, EngineError> {
    let path = run_dir.join("reviews.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ReviewEntry =
            serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;

        if let Some(threshold) = after_seq {
            if entry.seq > threshold {
                out.push(entry);
            }
        } else {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_read_reviews_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s0 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate".into(),
                decision: "approved".into(),
                note: "a".into(),
            },
        )
        .unwrap();
        let s1 = post_review(
            dir.path(),
            ReviewCommand {
                node: "gate2".into(),
                decision: "rejected".into(),
                note: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);

        let all = read_reviews_after(dir.path(), None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].cmd.node, "gate");

        let after = read_reviews_after(dir.path(), Some(s0)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].cmd.decision, "rejected");
    }
}
