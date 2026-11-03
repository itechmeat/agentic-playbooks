//! Unattended mode in the drive: every wait without a declared outcome ends
//! the run `failed` at once, through the unhandled-failure stop, so no
//! failure route is taken and no route leads into another wait.
//!
//! The mode is read from the run's write-once manifest ([`mode`]); the run
//! config only carries it for a run without one, so a rewrite of `run.yaml`
//! (an older apb, a resume) never turns the run attended.
//!
//! What each wait does:
//! - `human_review` without `unattended: { decide }`, a `publish` consent
//!   and a question without `default_answer` fail `hitl_required`;
//! - a `human_review` with `decide` is decided at once (after
//!   [`crate::gate::unattended::run_decide_refusal`] passes), the decision
//!   stamped `decided_by: unattended`, never a person's consent; a request
//!   left open by a drive that stopped before its decision is decided on
//!   resume, and a decision that cannot be recorded fails the run
//!   (`super::review_gate::post_unattended`), so the gate never waits;
//! - a question with `default_answer` is answered at once, `answered_by:
//!   unattended`;
//! - a budget refusal fails `budget_exhausted` instead of pausing (the
//!   gate records it, [`super::budget_gate::DriveGate::exhausted`]);
//! - a billing hold (work billed or of unknown basis the run holds no
//!   confirmation for, at an agent spawn or the supervisor's) fails
//!   `hitl_required` naming the node it held, instead of the billing pause
//!   only a person continues ([`billing_hold`], [`supervisor_held`]);
//! - a deferred retry never releases the driver
//!   ([`super::deferred::may_release`]);
//! - a sub-playbook child inherits the mode (prepare reads the parent's
//!   manifest) and its `hitl_required` or `budget_exhausted` stops the
//!   parent the same way ([`child_stop`]).

use std::path::Path;

use apb_core::schema::{NodeKind, Playbook};

use super::journal::{node_finished_count, node_started_count};
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, read_all};
use crate::pause::{PauseBy, PauseCause, PauseRequest};
use crate::run_config::{BUDGET_EXHAUSTED, HITL_REQUIRED, RunConfig};
use crate::state::NodeStatus;

/// The `answered_by` of a question an unattended run answered with its
/// default.
pub(crate) const ANSWERED_BY: &str = "unattended";

/// Whether the run at `run_dir` is unattended: its manifest's record, or
/// the run config's copy for a run without a manifest.
pub(crate) fn mode(run_dir: &Path, cfg: &RunConfig) -> bool {
    match crate::manifest::read(run_dir) {
        Ok(Some(m)) => m.unattended,
        _ => cfg.unattended,
    }
}

/// The run error reason of a wait of `kind` at `node` without an
/// unattended outcome.
pub(crate) fn hitl(kind: &str, node: &str) -> String {
    format!("{HITL_REQUIRED}: {kind} `{node}` has no unattended outcome")
}

/// Closes a visit `node` left open (a question, a list visit) as failed
/// with `reason`, before the unhandled-failure stop ends the run there.
pub(crate) fn close_open(
    log: &mut EventLog,
    run_dir: &Path,
    node: &str,
    reason: &str,
) -> Result<(), EngineError> {
    let events = read_all(run_dir)?;
    if node_started_count(&events, node) > node_finished_count(&events, node) {
        log.append(EventPayload::NodeFinished {
            node: node.to_string(),
            status: NodeStatus::Failed.as_str().into(),
            attempt: 1,
            output: reason.to_string(),
            artifacts: Vec::new(),
        })?;
    }
    Ok(())
}

/// What an unattended run does at the `human_review` gate `gate`.
pub(crate) enum Review {
    /// Post this decision and note.
    Decide { option: String, note: String },
    /// Fail the run with this reason.
    Fail(String),
}

/// The unattended outcome of the gate `gate`: its declared `decide`, when
/// the recheck over what the run would reach before the next gate passes;
/// else a `hitl_required` failure.
pub(crate) fn review(root: &Path, run_dir: &Path, playbook: &Playbook, gate: &str) -> Review {
    let Some(decide) = playbook
        .node(gate)
        .and_then(|n| n.unattended.as_ref())
        .and_then(|u| u.decide())
    else {
        return Review::Fail(hitl("human_review", gate));
    };
    let refusal =
        crate::gate::unattended::run_decide_refusal(root, run_dir, playbook, gate, &decide.decide);
    match refusal {
        Some(why) => Review::Fail(format!(
            "{HITL_REQUIRED}: human_review `{gate}` cannot decide `{}` unattended: {why}",
            decide.decide
        )),
        None => Review::Decide {
            option: decide.decide.clone(),
            note: decide.note.clone().unwrap_or_default(),
        },
    }
}

/// The question `node` asked, in an unattended run: its `default_answer`
/// to post now, or the `hitl_required` failure without one.
pub(crate) fn question(playbook: &Playbook, node: &str) -> Result<String, String> {
    match playbook.node(node).map(|n| &n.kind) {
        Some(NodeKind::AgentTask {
            default_answer: Some(answer),
            ..
        }) => Ok(answer.clone()),
        _ => Err(hitl("question", node)),
    }
}

/// The reason the parent stops with when the sub-playbook child that node
/// `node` ran last failed for want of a person or of budget:
/// `<code>: via child <run>: <the child's reason>`. `None` for any other
/// failure, which takes the parent's failure route as before.
pub(crate) fn child_stop(root: &Path, run_dir: &Path, node: &str) -> Option<String> {
    let events = read_all(run_dir).ok()?;
    let child = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { node_id, run_id } if node_id == node => {
            Some(run_id.clone())
        }
        _ => None,
    })?;
    // A child lives next to its parent.
    let _ = root;
    let child_events = read_all(&crate::run_config::sibling_run(run_dir, &child)).ok()?;
    let reason = child_events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    })?;
    [HITL_REQUIRED, BUDGET_EXHAUSTED]
        .into_iter()
        .find_map(|code| {
            let rest = reason.strip_prefix(code)?.strip_prefix(": ")?;
            Some(format!("{code}: via child {child}: {rest}"))
        })
}

/// The run error reason of an unattended run whose `pause` is a billing
/// pause at `node` (the billed or unknown-basis work it held); `None` for
/// any other pause, which applies as before.
pub(crate) fn billing_hold(pause: &PauseRequest, node: &str) -> Option<String> {
    let reason = pause.reason.as_deref()?;
    (crate::pause::cause_of(reason, pause.by.unwrap_or(PauseBy::Operator))
        == Some(PauseCause::Billing))
    .then(|| format!("{}: {reason}", hitl("billing hold", node)))
}

/// The engine id an unattended run fails at when the run's billing held
/// its supervisor (no node holds it).
pub(crate) const SUPERVISOR_NODE: &str = "@supervisor";

/// The run error reason of an unattended run whose supervisor the run's
/// billing held since the drive's latest start or resume; `None` when it
/// was not held.
pub(crate) fn supervisor_held(events: &[Event]) -> Option<String> {
    events
        .iter()
        .rev()
        .take_while(|e| {
            !matches!(
                e.payload,
                EventPayload::RunStarted { .. } | EventPayload::RunResumed { .. }
            )
        })
        .find_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. }
                if action == super::supervisor::SUPERVISOR_BILLING_HOLD =>
            {
                Some(format!(
                    "{}: {detail}",
                    hitl("billing hold of the supervisor", SUPERVISOR_NODE)
                ))
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_billing_pause_is_a_billing_hold() {
        let billing = crate::pause::billing::billing_pause(&apb_core::billing::BillingEntry {
            agent: "codex".into(),
            basis: apb_core::agents::CostBasis::Billed,
        });
        assert_eq!(
            billing_hold(&billing, "work").as_deref(),
            Some(
                "hitl_required: billing hold `work` has no unattended outcome: billing (codex: billed)"
            )
        );
        let quota = crate::pause::quota::quota_pause("codex", "m", 1);
        assert_eq!(billing_hold(&quota, "work"), None);
        let operator = PauseRequest {
            by: Some(PauseBy::Operator),
            ..billing.clone()
        };
        assert_eq!(billing_hold(&operator, "work"), None);
    }

    #[test]
    fn a_held_supervisor_counts_since_the_latest_start_or_resume() {
        let ev = |seq, payload| Event {
            seq,
            ts: 1,
            payload,
        };
        let hold = || EventPayload::SupervisorAction {
            action: super::super::supervisor::SUPERVISOR_BILLING_HOLD.into(),
            node: None,
            detail: "claude: billed".into(),
        };
        let started = || {
            serde_json::from_value::<EventPayload>(serde_json::json!({
                "type": "run_started", "playbook": "p", "version": "1.0.0"
            }))
            .unwrap()
        };
        let held = [ev(0, started()), ev(1, hold())];
        assert_eq!(
            supervisor_held(&held).as_deref(),
            Some(
                "hitl_required: billing hold of the supervisor `@supervisor` has no unattended outcome: claude: billed"
            )
        );
        assert_eq!(supervisor_held(&[ev(0, hold()), ev(1, started())]), None);
    }
}
