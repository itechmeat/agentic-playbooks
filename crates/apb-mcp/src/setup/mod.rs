//! Agent-first setup: `apb setup plan|apply|remove` and the MCP tools
//! `setup_plan` and `setup_howto`.
//!
//! An installing agent reads the plan ([`plan`], read only), runs exactly
//! that plan by its digest ([`apply`]) and can take it all back
//! ([`remove`]). Setup installs the default playbooks and skills apb ships,
//! writes the role profiles they bind to from the agents this machine has,
//! registers the MCP server in the detected hosts and, for a project,
//! writes the standing playbook instruction. Everything it does is local,
//! listed in the plan and recorded. What loosens a guarantee or sends
//! anything out (lowering a launch floor, the feedback loop, notification
//! targets, trusting a fork) is never done: the plan lists it under
//! `needs_person`.

pub mod hook;
pub mod hosts;
pub mod record;
pub mod standing;

mod apply;
mod doctor;
mod observe;
mod plan;
mod remove;
#[cfg(test)]
mod tests;
mod upgrade;

pub use apply::{ApplyOutcome, ApplyStatus, Done, Failed, apply};
pub use doctor::{DoctorLine, doctor_lines};
pub use observe::{AVAILABLE as TRACKER_AVAILABLE, Suggestion, TrackerSeen};
pub use plan::{
    Action, ActionKind, ManualStep, NEEDS_PERSON, NeedsPerson, Observed, PLAN_SCHEMA, Plan,
    Refusal, ReplaceModified, Warning, plan,
};
pub use remove::{Kept, RemoveOutcome, Removed, remove};
pub use upgrade::reconcile_if_set_up;

/// The refusal code of setup inside a run step.
pub const RUN_STEP: &str = "run_step";

/// Why setup's writes (apply, remove) are refused in this process: it is a
/// step of a run (`APB_RUN_ID` or `APB_HOST_TASK` is set). Installing and
/// configuring apb is the person's or their own agent session's, never a
/// playbook step's; reading the plan is allowed. `None` outside a run.
pub fn step_refusal() -> Option<String> {
    apb_core::step_env::StepCaller::from_env()
        .describe()
        .map(|from| {
            format!(
                "setup apply and remove are refused {from}: a run step does not install, configure or remove apb (it may read apb setup plan); run them from the person's own session"
            )
        })
}

/// The installing agent's procedure, `docs/AGENT-SETUP.md`: served by the
/// MCP tool `setup_howto` and linked first from `llms.txt`.
pub const HOWTO: &str = include_str!("../../../../docs/AGENT-SETUP.md");
