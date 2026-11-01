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

/// The next actions of the observed run and its live children.
pub fn next_actions(obs: &Observation) -> RunActions {
    let waits = verdict::collect(obs);
    let stop = verdict::stop_verdict(obs, &waits);
    let actions: Vec<NextAction> = waits.into_iter().map(|w| w.action).collect();
    RunActions {
        observed_at_ms: obs.now_ms,
        status: obs.status(),
        stop,
        primary: (!actions.is_empty()).then_some(0),
        actions,
        children_truncated: obs.truncated(),
    }
}
