//! Which subscription a run gets: the run's own request first, then the
//! project, then the machine. A pure function, so every launch surface and
//! the engine read the same answer.
//!
//! - A run that asks (`NotifyRequest::On`) notifies, to its own target or
//!   the machine's, about its own transitions and outcomes or the narrowed
//!   default. The playbook filter does not apply to a run that asked.
//! - A run that asks for nothing follows the machine default, unless the
//!   project turned it off or a `playbooks` filter of `policy.yaml` leaves
//!   its playbook out. The project may narrow the transitions and the
//!   outcomes.
//! - `NotifyRequest::Off` means no subscription, and so does a default whose
//!   transition list is empty once narrowed. A run that asked keeps its
//!   empty list, so the launch check refuses it instead of dropping it.

use serde::Serialize;

use super::{
    FinishOutcome, NotifyRequest, NotifySettings, NotifyTarget, ProjectNotify, Toggle, Transition,
};
use crate::policy_file::{PlaybookKey, Policies};

/// Where the subscription in force came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifySource {
    /// The run's own request at start.
    Run,
    /// The machine default, narrowed by the project.
    Project,
    /// The machine default.
    Machine,
    /// A person changed it on the live run.
    Person,
    /// Inherited from the parent run of a sub-playbook.
    Parent,
}

impl NotifySource {
    pub fn as_str(self) -> &'static str {
        match self {
            NotifySource::Run => "run",
            NotifySource::Project => "project",
            NotifySource::Machine => "machine",
            NotifySource::Person => "person",
            NotifySource::Parent => "parent",
        }
    }

    /// The inverse of [`NotifySource::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        [
            NotifySource::Run,
            NotifySource::Project,
            NotifySource::Machine,
            NotifySource::Person,
            NotifySource::Parent,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    /// Whether a person asked for this subscription explicitly, so a target
    /// that cannot be used refuses the request instead of being skipped.
    pub fn is_explicit(self) -> bool {
        matches!(self, NotifySource::Run | NotifySource::Person)
    }
}

/// The subscription in force. `target` is `None` when nothing names one,
/// which the launch check reports as a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub target: Option<NotifyTarget>,
    pub on: Vec<Transition>,
    /// The outcomes a `finished` notice is sent for.
    pub finished: Vec<FinishOutcome>,
    pub source: NotifySource,
}

/// Every setting a subscription is decided from, besides the run's own
/// request.
#[derive(Debug, Clone, Copy)]
pub struct Settings<'a> {
    /// The machine's `notify:` of `config.yaml`.
    pub machine: &'a NotifySettings,
    /// The project's `notify:` of `.apb/config.yaml`.
    pub project: &'a ProjectNotify,
    /// The filters of both `policy.yaml` files.
    pub policies: &'a Policies,
}

/// The subscription of a run of `playbook` from the settings and the run's
/// own request; `None` for a run that does not notify. A run's own request
/// always comes back, even when no transition is left once narrowed: the
/// caller refuses that instead of dropping it. Without a `playbook` (a
/// check that does not know it yet) the `playbooks` filters pass.
pub fn effective(
    settings: Settings<'_>,
    playbook: Option<&PlaybookKey>,
    request: Option<&NotifyRequest>,
) -> Option<Effective> {
    let Settings {
        machine,
        project,
        policies,
    } = settings;
    let narrowed = || match &project.on {
        Some(only) => machine
            .on
            .iter()
            .copied()
            .filter(|t| only.contains(t))
            .collect(),
        None => machine.on.clone(),
    };
    let (target, on, finished, source) = match request {
        Some(NotifyRequest::Off) => return None,
        Some(NotifyRequest::On {
            target,
            on,
            finished,
        }) => (
            target.clone().or_else(|| machine.target.clone()),
            on.clone().unwrap_or_else(narrowed),
            finished.clone().unwrap_or_else(|| policies.finished()),
            NotifySource::Run,
        ),
        None => {
            if machine.default == Toggle::Off
                || project.enabled == Some(false)
                || playbook.is_some_and(|p| !policies.covers(p))
            {
                return None;
            }
            let source = if project.on.is_some() {
                NotifySource::Project
            } else {
                NotifySource::Machine
            };
            (
                machine.target.clone(),
                narrowed(),
                policies.finished(),
                source,
            )
        }
    };
    let finished = FinishOutcome::normalized(&finished);
    // Filters that leave no outcome leave no `finished` notice either.
    let on: Vec<Transition> = Transition::normalized(&on)
        .into_iter()
        .filter(|t| *t != Transition::Finished || !finished.is_empty())
        .collect();
    (!on.is_empty() || source.is_explicit()).then_some(Effective {
        target,
        on,
        finished,
        source,
    })
}
