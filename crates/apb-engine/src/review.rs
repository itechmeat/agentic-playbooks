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
    /// The nonce of the plan a `publish` decision answers (see
    /// `apb_core::publish::nonce`), stamped by [`post_decision`]. `None` for
    /// every `human_review` decision and in old channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
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
/// waiting. A publish gate is the exception: its consent is consumed only by
/// the nonce this check stamps, so a decision for a publish node whose
/// journal or channel cannot be read now is a conflict to retry, never an
/// unstamped entry no drive would consume. Without the snapshot the journal
/// tells a publish node by the plan it journaled. Who decides does not
/// depend on the snapshot: a `publish` consent from inside a run step is
/// refused for a publish node, and for any node when the snapshot cannot
/// say what the node is.
fn check_review_target(
    run_dir: &Path,
    cmd: &ReviewCommand,
    nonce: Option<String>,
) -> Result<Option<String>, EngineError> {
    use apb_core::publish::OPTION_PUBLISH;
    use apb_core::schema::NodeKind;

    let node = cmd.node.as_str();
    let consent = cmd.decision == OPTION_PUBLISH;
    // Whether the node is a publish node, and whether a snapshot said so.
    let (publish, snapshot) = match crate::progress::load_run_playbook(run_dir) {
        Some(playbook) => {
            let kind = playbook.node(node).map(|n| &n.kind);
            let publish = matches!(kind, Some(NodeKind::Publish { .. }));
            if !publish && !matches!(kind, Some(NodeKind::HumanReview { .. })) {
                return Err(EngineError::NotFound(format!(
                    "node `{node}` is not a human_review or publish node of this run's playbook `{}`",
                    playbook.id
                )));
            }
            if !publish && nonce.is_some() {
                return Err(EngineError::Invalid(format!(
                    "node `{node}` is a human_review node; a nonce answers a publish node only"
                )));
            }
            (publish, true)
        }
        None => {
            if consent || nonce.is_some() {
                refuse_step_consent()?;
            }
            match crate::event::read_all(run_dir) {
                Ok(events) => (crate::publish::latest_plan(&events, node).is_some(), false),
                Err(_) if consent || nonce.is_some() => {
                    return Err(EngineError::Conflict(format!(
                        "the journal of node `{node}` cannot be read right now; decide again"
                    )));
                }
                // Nothing to judge by: accept, as before snapshots existed.
                Err(_) => return Ok(nonce),
            }
        }
    };
    if publish {
        check_publish_decision(cmd)?;
        if consent {
            refuse_step_consent()?;
        }
    } else if !snapshot {
        return Ok(nonce);
    }

    let unreadable = |what: &str| -> Result<Option<String>, EngineError> {
        if publish {
            Err(EngineError::Conflict(format!(
                "the {what} of publish node `{node}` cannot be read right now; decide again"
            )))
        } else {
            Ok(nonce.clone())
        }
    };
    let Ok(events) = crate::event::read_all(run_dir) else {
        return unreadable("journal");
    };
    let open = crate::event::review_open_count(&events, node);
    let decided = crate::event::review_decided_count(&events, node);
    if open == 0 {
        return Err(EngineError::Conflict(format!(
            "node `{node}` has no review decision pending"
        )));
    }
    let Ok(queued) = read_reviews_after(run_dir, None) else {
        return unreadable("review channel");
    };
    if publish {
        return check_publish_nonce(&events, &queued, node, cmd, nonce);
    }

    // The journal alone cannot see a decision that is already sitting in the
    // channel and has not been folded into a `ReviewDecided` event yet, so
    // requested-vs-decided would accept a second decision for the same open
    // request. The drive consumes the `decided`-th record for the node
    // (`scheduler::review_gate`, `Consume::Nth`), which is exactly the count
    // this reads back: everything past that is queued and unconsumed. Once
    // the queue already answers every outstanding request, another decision
    // is not an answer, it is a stale extra a cyclic gate would consume on
    // its NEXT visit without anyone confirming it.
    let unconsumed = queued
        .iter()
        .filter(|e| e.cmd.node == node)
        .count()
        .saturating_sub(decided);
    if unconsumed >= open {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for node `{node}`"
        )));
    }
    Ok(None)
}

/// A publish decision is `publish` or `skip` (a run step's `publish` is
/// already refused by [`refuse_step_consent`]).
fn check_publish_decision(cmd: &ReviewCommand) -> Result<(), EngineError> {
    use apb_core::publish::{OPTIONS, is_option};
    if !is_option(&cmd.decision) {
        return Err(EngineError::Invalid(format!(
            "`{}` is not a decision of publish node `{}`; choose one of {}",
            cmd.decision,
            cmd.node,
            OPTIONS.join(", ")
        )));
    }
    Ok(())
}

/// A publish consent is a person's: one posted from inside a run step
/// (`APB_RUN_ID` or `APB_HOST_TASK` set in the posting process) is refused,
/// whatever the run's snapshot says.
fn refuse_step_consent() -> Result<(), EngineError> {
    match apb_core::step_env::StepCaller::from_env().refusal("publishing") {
        Some(why) => Err(EngineError::Invalid(why)),
        None => Ok(()),
    }
}

/// A publish decision answers exactly the open plan: `publish` must carry
/// its nonce, a `skip` without one is stamped with it, a stale nonce is a
/// conflict naming the current plan, and a second decision for the same
/// nonce is refused. The drive consumes by nonce, so a decision queued for
/// a replaced plan never answers a different one. The nonce names the plan,
/// not the request: a decision queued just before a supervisor withdrew the
/// request answers a later plan only when it is identical (the same push to
/// the same remote state), which is the push the person consented to.
fn check_publish_nonce(
    events: &[crate::event::Event],
    queued: &[ReviewEntry],
    node: &str,
    cmd: &ReviewCommand,
    nonce: Option<String>,
) -> Result<Option<String>, EngineError> {
    use crate::event::EventPayload;
    let Some(EventPayload::PublishPlanned {
        nonce: open,
        plan_digest,
        branch,
        after,
        url,
        ..
    }) = crate::publish::latest_plan(events, node).map(|e| &e.payload)
    else {
        return Err(EngineError::Conflict(format!(
            "publish node `{node}` has no plan pending"
        )));
    };
    let given = match nonce {
        Some(n) => n,
        None if cmd.decision == apb_core::publish::OPTION_SKIP => open.clone(),
        None => {
            return Err(EngineError::Invalid(format!(
                "publishing needs the nonce of the plan the person saw; the current plan of node `{node}` pushes {after} to `{url}` branch `{branch}` with nonce `{open}`"
            )));
        }
    };
    if given != *open {
        return Err(EngineError::Conflict(format!(
            "nonce `{given}` does not answer the current plan of publish node `{node}`: it pushes {after} to `{url}` branch `{branch}` with nonce `{open}`; show the person that plan and decide again"
        )));
    }
    let decided = events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::ReviewDecided { node: n, plan_digest: Some(d), .. }
                if n == node && d == plan_digest)
        })
        .count();
    let queued_for_nonce = queued
        .iter()
        .filter(|e| e.cmd.node == node && e.nonce.as_deref() == Some(open.as_str()))
        .count();
    if queued_for_nonce > decided {
        return Err(EngineError::Conflict(format!(
            "a decision is already queued for the plan `{open}` of node `{node}`"
        )));
    }
    Ok(Some(given))
}

/// Queues a decision without a nonce: every `human_review` surface.
pub fn post_review(run_dir: &Path, cmd: ReviewCommand) -> Result<u64, EngineError> {
    post_decision(run_dir, cmd, None)
}

/// Queues a decision, with the nonce of the plan it answers for a
/// `publish` gate. The single entry point of every decision surface.
pub fn post_decision(
    run_dir: &Path,
    cmd: ReviewCommand,
    nonce: Option<String>,
) -> Result<u64, EngineError> {
    crate::driver::ensure_run_dir(run_dir)?;
    // The check and the append under one lock: two posters (a person and
    // the engine's automatic decision, issue #165 Part 14.4) that both pass
    // the check would queue two decisions for one visit, and the second
    // would decide the gate's next visit unseen.
    let _lock = apb_core::fsutil::lock_dir(run_dir, "reviews.jsonl.lock")?;
    let nonce = check_review_target(run_dir, &cmd, nonce)?;

    let seq = read_reviews_after(run_dir, None)?.len() as u64;

    let entry = ReviewEntry { seq, cmd, nonce };
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
