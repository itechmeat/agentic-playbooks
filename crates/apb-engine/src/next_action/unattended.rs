//! What an unattended run does at each wait ([`NextAction::unattended`]),
//! computed for every run, attended or not, so a CI author sees what the
//! mode would do. It reads the action and the definition of the run it acts
//! on; the drive's own rules are in `crate::scheduler::unattended`.
//!
//! An action that is not a wait (a send intent to resolve, the report, the
//! resume of an operator pause or of a dead driver) carries none.

use apb_core::schema::{NodeKind, WaitFor};

use super::observation::Observation;
use super::types::{ActionCode, AutoOutcome, NextAction, UnattendedOutcome};
use crate::pause::PauseCause;
use crate::run_config::{BUDGET_EXHAUSTED, HITL_REQUIRED, UNATTENDED_UNSUPPORTED};

/// Sets [`NextAction::unattended`] on every action of `actions`, each read
/// against the observed run it acts on (`obs` or one of its children).
pub(super) fn annotate(obs: &Observation, actions: &mut [NextAction]) {
    let runs = obs.runs();
    for a in actions {
        let run = runs.iter().find(|r| r.run_id == a.run_id).copied();
        a.unattended = run.and_then(|r| outcome(r, a));
    }
}

fn fails(code: &str) -> Option<UnattendedOutcome> {
    Some(UnattendedOutcome::FailsRun { code: code.into() })
}

/// Whether an unattended run of `run` would decide gate `gate` with
/// `option`: the drive's recheck over the run's pins and the connectors
/// installed here passes. A run whose project root cannot be told fails.
fn decides(run: &Observation, gate: &Option<String>, option: &str) -> bool {
    let (Some(gate), Some(playbook)) = (gate.as_deref(), run.playbook.as_ref()) else {
        return false;
    };
    crate::run_config::project_root_of(&run.run_dir).is_some_and(|root| {
        crate::gate::unattended::run_decide_refusal(&root, &run.run_dir, playbook, gate, option)
            .is_none()
    })
}

/// The unattended outcome of `a`, an action of `run`.
fn outcome(run: &Observation, a: &NextAction) -> Option<UnattendedOutcome> {
    let node = a
        .target
        .node
        .as_deref()
        .and_then(|id| run.playbook.as_ref()?.node(id));
    match a.code {
        ActionCode::AnswerQuestion => match node.map(|n| &n.kind) {
            Some(NodeKind::AgentTask {
                default_answer: Some(_),
                ..
            }) => Some(UnattendedOutcome::AnswersDefault),
            _ => fails(HITL_REQUIRED),
        },
        // The declared decision, when the drive's own recheck would let it
        // stand (a pinned child or a connector in reach may refuse it).
        ActionCode::DecideReview => match node.and_then(|n| n.unattended.as_ref()?.decide()) {
            Some(d) if decides(run, &a.target.node, &d.decide) => {
                Some(UnattendedOutcome::Decides {
                    option: d.decide.clone(),
                })
            }
            _ => fails(HITL_REQUIRED),
        },
        ActionCode::DecidePublish => fails(HITL_REQUIRED),
        ActionCode::ResumeWithBudget => fails(BUDGET_EXHAUSTED),
        ActionCode::SubmitHostTask
        | ActionCode::SupervisorRetryNode
        | ActionCode::SupervisorContinueFrom
        | ActionCode::SupervisorAbort => Some(UnattendedOutcome::RefusedAtStart {
            code: UNATTENDED_UNSUPPORTED.into(),
        }),
        ActionCode::RetryNow => Some(UnattendedOutcome::Proceeds {
            auto: AutoOutcome::RetryAttempt,
        }),
        ActionCode::WaitAgain => {
            let auto = a
                .auto
                .map(|p| p.outcome())
                .or(match node.map(|n| &n.kind) {
                    Some(NodeKind::Wait {
                        wait_for: WaitFor::Timer { .. },
                        ..
                    }) => Some(AutoOutcome::TimerElapses),
                    Some(NodeKind::Wait { .. }) => Some(AutoOutcome::WaitTimesOut),
                    _ => None,
                })?;
            Some(UnattendedOutcome::Proceeds { auto })
        }
        // A quota pause resumes at its `until` as in an attended run; an
        // operator's pause and a dead driver are no wait of the run.
        ActionCode::ResumeRun => (run.view.pause().and_then(|p| p.cause)
            == Some(PauseCause::Quota))
        .then_some(UnattendedOutcome::Proceeds {
            auto: AutoOutcome::ResumeAttempt,
        }),
        ActionCode::ResolveIntent | ActionCode::ReadReport => None,
    }
}
