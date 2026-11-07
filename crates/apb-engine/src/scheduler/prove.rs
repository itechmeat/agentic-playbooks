//! A check that must fail first (spec node-contracts D4): a
//! `success_check` with `prove_fails_first: true` runs once before the
//! first attempt of an effect scope, as an observation, and its
//! `check_proved` decides the visit until the node finishes.

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::exec_key::ExecKey;
use std::path::Path;

/// What a green proof decides: the node fails and the run needs a look, or
/// the node settles as already done. Mirrors
/// [`apb_core::schema::OnGreenBefore`]; carried through the journal as a
/// string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Green {
    Fail,
    Skip,
}

/// The result an open `check_proved` still carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Proved {
    /// The check failed before the work: a later pass can be told from no
    /// work, so the attempt runs.
    FailedFirst,
    /// The check passed before the work, and this is what it decides.
    GreenBefore(Green),
    /// The check changed the tree, or the comparison could not show it did
    /// not: the node fails (a crash before its `node_finished` resumes to
    /// the same failure).
    ChangedTree,
}

/// What the journal says about the proof of `key` now: due, or the open
/// `check_proved` that still decides the visit.
#[derive(Debug, Clone)]
pub(crate) enum ProofState {
    /// The proof already ran for this execution; the script is not run
    /// again (a retry, a fallback step, a deferred retry, a resume, a
    /// failure route back into a node that already ran an attempt).
    NotDue,
    /// The first attempt of an effect scope: the proof runs.
    Due,
    /// A crash left a `check_proved` with no `node_finished` after it: it
    /// decides, the script is not run again.
    Open(Proved),
}

/// The proof's effect scope floor: for a plain node (a proof never runs on
/// a list item) the one [`crate::connector::intent::ledger::
/// effect_scope_floor`] computes for `key.item = None` - the seq after the
/// node's last succeeded `node_finished` - read here without the send
/// ledger, whose import would tie this module into the scheduler's cycle.
fn scope_floor(events: &[Event], node: &str) -> u64 {
    events
        .iter()
        .rev()
        .find(|e| {
            matches!(&e.payload, EventPayload::NodeFinished { node: n, status, .. } if n == node && status == "succeeded")
        })
        .map_or(0, |e| e.seq + 1)
}

pub(crate) fn state(events: &[Event], key: ExecKey<'_>) -> ProofState {
    let floor = scope_floor(events, key.node);
    let in_scope = |e: &Event| e.seq >= floor;
    // The latest proof of the node with no `node_finished` of the node
    // after it is open (a crash left it mid-execution). A red proof goes on
    // to the attempt; a green one settles as it said.
    if let Some(e) = events.iter().rev().find(|e| {
        in_scope(e)
            && matches!(&e.payload, EventPayload::CheckProved { node, .. } if node == key.node)
    }) {
        let finished = events.iter().any(|f| {
            f.seq > e.seq
                && matches!(&f.payload, EventPayload::NodeFinished { node, .. } if node == key.node)
        });
        if !finished {
            let EventPayload::CheckProved {
                failed_first,
                on_green_before,
                changed_tree,
                ..
            } = &e.payload
            else {
                unreachable!("filtered to CheckProved above")
            };
            let proved = if *changed_tree {
                Proved::ChangedTree
            } else if *failed_first {
                Proved::FailedFirst
            } else {
                let green = match on_green_before.as_deref() {
                    Some("skip") => Green::Skip,
                    _ => Green::Fail,
                };
                Proved::GreenBefore(green)
            };
            return ProofState::Open(proved);
        }
    }
    // An attempt of this key in scope: the proof already had its chance.
    if events
        .iter()
        .any(|e| in_scope(e) && matches!(&e.payload, EventPayload::AttemptStarted { node, item, .. } if node == key.node && item.as_deref() == key.item))
    {
        return ProofState::NotDue;
    }
    ProofState::Due
}

/// Runs the proof: the check script once, inside the placement, between a
/// before and an after tree state of the node's tree. A changed tree, or a
/// partial comparison, fails the node at once (`Err(reason)`); the caller
/// journals `check_proved` with what came out.
///
/// The proof is not an agent attempt: no `attempt_started` or
/// `attempt_finished` is journaled for it.
/// The node fails when its proof changed the tree, or when the comparison
/// cannot show it did not.
pub(crate) const CHECK_CHANGED_TREE: &str = "check_changed_tree";

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    journal: &crate::scheduler::journal::Journal,
    run_dir: &Path,
    script: &str,
    node_id: &str,
    node_dir: &Path,
    workdir: &Path,
    sandbox: Option<&crate::sandbox::SandboxPlan>,
    green: apb_core::schema::OnGreenBefore,
) -> Result<Result<Proved, String>, EngineError> {
    // The proof is an observation: the tree state before, the check, the
    // tree state after. The read-only state's one comparison answers both.
    let before = crate::scheduler::read_only::Before::take(node_id, 0, node_dir, None);
    let r = crate::script::run_check(run_dir, workdir, script, sandbox)?;
    let compared = before.compare();
    // A sandboxed check that changed what redirects git ends the node like
    // a changed tree: the agent must not start on a redirected git.
    let git = sandbox.and_then(crate::sandbox::SandboxPlan::check_git);
    let changed = !compared.changes.is_empty();
    if changed || compared.partial().is_some() || git.is_some() {
        let what = if let Some(v) = git {
            format!(
                "changed the tree while proving it fails first: {}",
                v.reason
            )
        } else if let Some(limit) = compared.partial() {
            format!(
                "cannot be shown to leave the tree alone: the tree is too large to compare whole ({})",
                super::read_only::limit_words(limit)
            )
        } else {
            let first = compared
                .changes
                .first()
                .map(|c| format!("{} {}", c.path, c.kind.as_str()))
                .unwrap_or_default();
            format!("changed the tree while proving it fails first: {first}")
        };
        journal.append(EventPayload::CheckProved {
            node: node_id.to_string(),
            script: script.to_string(),
            failed_first: false,
            on_green_before: None,
            changed_tree: true,
        })?;
        return Ok(Err(format!(
            "{CHECK_CHANGED_TREE}: success_check `{script}` {what}"
        )));
    }
    let failed_first = r.status != crate::state::NodeStatus::Succeeded;
    journal.append(EventPayload::CheckProved {
        node: node_id.to_string(),
        script: script.to_string(),
        failed_first,
        on_green_before: (!failed_first).then(|| match green {
            apb_core::schema::OnGreenBefore::Fail => "fail".to_string(),
            apb_core::schema::OnGreenBefore::Skip => "skip".to_string(),
        }),
        changed_tree: false,
    })?;
    Ok(Ok(if failed_first {
        Proved::FailedFirst
    } else {
        Proved::GreenBefore(match green {
            apb_core::schema::OnGreenBefore::Fail => Green::Fail,
            apb_core::schema::OnGreenBefore::Skip => Green::Skip,
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_with(events: Vec<EventPayload>) -> Vec<Event> {
        events
            .into_iter()
            .enumerate()
            .map(|(i, payload)| Event {
                seq: i as u64 + 1,
                ts: 0,
                payload,
            })
            .collect()
    }

    fn payload(json: &str) -> EventPayload {
        serde_json::from_str(json).unwrap()
    }

    fn proved(failed_first: bool, on_green: Option<&str>) -> EventPayload {
        let green = on_green
            .map(|g| format!(r#","on_green_before":"{g}""#))
            .unwrap_or_default();
        payload(&format!(
            "{{\"type\":\"check_proved\",\"node\":\"w\",\"script\":\"scripts/x.sh\",\"failed_first\":{failed_first}{green}}}"
        ))
    }

    fn attempt(node: &str) -> EventPayload {
        payload(&format!(
            "{{\"type\":\"attempt_started\",\"node\":\"{node}\",\"attempt\":1,\"agent\":\"stub\"}}"
        ))
    }

    fn finished(node: &str, status: &str) -> EventPayload {
        payload(&format!(
            "{{\"type\":\"node_finished\",\"node\":\"{node}\",\"status\":\"{status}\",\"attempt\":1,\"output\":\"\"}}"
        ))
    }

    const KEY: ExecKey<'_> = ExecKey {
        node: "w",
        item: None,
    };

    #[test]
    fn no_attempt_in_scope_is_due() {
        let events = journal_with(vec![attempt("other")]);
        assert!(matches!(state(&events, KEY), ProofState::Due));
    }

    #[test]
    fn an_attempt_since_the_floor_is_not_due() {
        let events = journal_with(vec![attempt("w")]);
        assert!(matches!(state(&events, KEY), ProofState::NotDue));
    }

    #[test]
    fn a_proof_with_no_node_finished_after_it_is_open() {
        let events = journal_with(vec![proved(true, None)]);
        match state(&events, KEY) {
            ProofState::Open(Proved::FailedFirst) => {}
            other => panic!("expected an open red proof, got {other:?}"),
        }
        let events = journal_with(vec![proved(false, Some("skip"))]);
        match state(&events, KEY) {
            ProofState::Open(Proved::GreenBefore(Green::Skip)) => {}
            other => panic!("expected an open green skip proof, got {other:?}"),
        }
    }

    #[test]
    fn a_loop_visit_after_a_success_is_due_again() {
        let events = journal_with(vec![
            proved(true, None),
            attempt("w"),
            finished("w", "succeeded"),
            attempt("w"),
        ]);
        // The success raises the floor above the earlier proof and attempt;
        // the second attempt_started is at or above it, so it is NotDue.
        assert!(matches!(state(&events, KEY), ProofState::NotDue));
        // Without the second attempt, the new visit is due.
        let events = journal_with(vec![
            proved(true, None),
            attempt("w"),
            finished("w", "succeeded"),
        ]);
        assert!(matches!(state(&events, KEY), ProofState::Due));
    }

    #[test]
    fn a_green_fail_proof_followed_by_its_node_finished_then_a_retry_is_due() {
        // The green-before failure journals the proof and the node's
        // failure, and no attempt; a supervisor retry of it proves again.
        let events = journal_with(vec![
            proved(false, Some("fail")),
            finished("w", "failed"),
            attempt("other"),
        ]);
        assert!(matches!(state(&events, KEY), ProofState::Due));
    }
}
