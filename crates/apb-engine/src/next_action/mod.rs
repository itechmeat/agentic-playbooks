//! Next actions: what a run is ready for right now, derived from one
//! [`Observation`] of it and its live sub-playbook children.
//!
//! Every open wait of the run (a question, a human review, a publish gate,
//! a host task, a supervisor park, a `wait` node, a pause, a dead driver)
//! becomes one or more [`NextAction`]s, each naming the run it acts on, the
//! request it answers, its arguments, who the run accepts it from and what
//! happens without anyone ([`AutoProgress`]). [`RunActions::stop`] is one
//! verdict over all of them (see [`verdict`]) and `primary` the action a
//! surface leads with.
//!
//! The engine never builds a command line or a tool call: each surface maps
//! [`ActionCode`] with an exhaustive `match`. What a given caller may do is
//! the surface's question; `principals` only says whom the run accepts an
//! action from, and the path that executes it checks everything again.
//!
//! Starts that are refused before a run exists have their own type,
//! [`start::StartAction`].

pub mod command;
pub mod deadlines;
pub mod observation;
pub mod start;
pub(crate) mod start_action;
mod types;
mod unattended;
pub mod verdict;

#[cfg(test)]
mod tests;

pub use observation::{ChildObservation, MAX_CHILD_DEPTH, Observation};
pub use types::*;

/// The next actions as a surface shows them: [`next_actions`] with every
/// action's texts redacted by the secret values of the run it acts on (a
/// child's by its own and its ancestors'), before any surface clips or
/// renders them.
pub fn for_surface(
    redaction: &crate::run_redact::WorkspaceRedaction,
    obs: &Observation,
) -> RunActions {
    let mut actions = next_actions(obs);
    let mut redactors = std::collections::BTreeMap::new();
    for a in &mut actions.actions {
        let r = redactors
            .entry(a.run_id.clone())
            .or_insert_with(|| redaction.run(&a.run_id));
        a.scrub_texts(|s| r.redact_str(s));
        a.command = command::render(a, &std::collections::BTreeMap::new()).map(|c| c.line());
    }
    actions
}

/// Why a caller posting through `channel` (from inside the run step
/// `step`, if any) does not take `a`, the action of the run at `run_dir`:
/// a step never decides a gate, answers a question or resolves a send of
/// its own run tree (`run_step_own_run`), a dashboard without keys gives
/// no publish consent on a publish gate (`keyless_dashboard`, the decision
/// path's [`crate::review::channel::refuses_publish_consent`]), and a
/// person-only
/// gate refuses every channel that is not a person's. `None` when the caller may take
/// it; the path that executes it checks everything again.
pub fn caller_refusal(
    a: &NextAction,
    run_dir: &std::path::Path,
    channel: crate::decision_channel::DecisionChannel,
    step: &apb_core::step_env::StepCaller,
) -> Option<CallerRefusal> {
    let decides = matches!(
        a.code,
        ActionCode::DecideReview
            | ActionCode::DecidePublish
            | ActionCode::AnswerQuestion
            | ActionCode::ResolveIntent
    );
    if decides && crate::review::channel::refuse_own_run(run_dir, step).is_err() {
        return Some(CallerRefusal::RunStepOwnRun);
    }
    // The decision path's own rule (`review::channel`): a dashboard
    // without keys takes no publish consent on a publish gate, so the
    // action is not this caller's to consent to (a skip still goes
    // through, and an option named `publish` on a `human_review` gate is
    // an ordinary review decision).
    if a.code == ActionCode::DecidePublish
        && crate::review::channel::refuses_publish_consent(
            channel,
            apb_core::publish::OPTION_PUBLISH,
            crate::review::channel::is_publish_gate(
                run_dir,
                a.target.node.as_deref().unwrap_or_default(),
            ),
        )
    {
        return Some(CallerRefusal::KeylessDashboard);
    }
    let gate = matches!(a.code, ActionCode::DecideReview | ActionCode::DecidePublish);
    if gate {
        // One predicate with the engine's (`review::passkey_required`): a
        // person-only gate, a publish consent and a bound approval all need
        // a fresh assertion while a passkey is enforced, which only the
        // dashboard can give. A scenario's script is exempt, as it is on
        // the append path.
        let cmd = crate::review::ReviewCommand {
            node: a.target.node.clone().unwrap_or_default(),
            decision: String::new(),
            note: String::new(),
        };
        if crate::review::passkey_required(run_dir, &cmd)
            && channel != crate::decision_channel::DecisionChannel::DashboardSession
            && channel != crate::decision_channel::DecisionChannel::DashboardKeyless
            && channel != crate::decision_channel::DecisionChannel::Scenario
        {
            return Some(CallerRefusal::PasskeyRequired);
        }
    }
    if gate && a.person_only {
        return channel.person_only_refusal();
    }
    None
}

/// Marks every action of `actions` with [`caller_refusal`] for a caller
/// posting through `channel` from `step`; `runs_dir` holds the runs the
/// actions name. Every surface marks the actions it shows.
pub fn mark_for_caller(
    actions: &mut RunActions,
    runs_dir: &std::path::Path,
    channel: crate::decision_channel::DecisionChannel,
    step: &apb_core::step_env::StepCaller,
) {
    for a in &mut actions.actions {
        let dir = runs_dir.join(&a.run_id);
        a.caller_refusal = caller_refusal(a, &dir, channel, step);
    }
}

/// The next actions of the observed run and its live children.
pub fn next_actions(obs: &Observation) -> RunActions {
    let waits = verdict::collect(obs);
    let stop = verdict::stop_verdict(obs, &waits);
    let mut actions: Vec<NextAction> = waits.into_iter().map(|w| w.action).collect();
    unattended::annotate(obs, &mut actions);
    RunActions {
        observed_at_ms: obs.now_ms,
        status: obs.status(),
        stop,
        primary: (!actions.is_empty()).then_some(0),
        actions,
        children_truncated: obs.truncated(),
    }
}
