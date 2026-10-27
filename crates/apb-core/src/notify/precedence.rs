//! Which subscription a run gets: the run's own request first, then the
//! project, then the machine. A pure function, so every launch surface and
//! the engine read the same answer.
//!
//! - A run that asks (`NotifyRequest::On`) notifies, to its own target or
//!   the machine's, about its own transitions or the narrowed default.
//! - A run that asks for nothing follows the machine default, unless the
//!   project turned it off. The project may narrow the transitions.
//! - `NotifyRequest::Off` means no subscription, and so does a default whose
//!   transition list is empty once narrowed. A run that asked keeps its
//!   empty list, so the launch check refuses it instead of dropping it.

use serde::Serialize;

use super::{NotifyRequest, NotifySettings, NotifyTarget, ProjectNotify, Toggle, Transition};

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
    pub source: NotifySource,
}

/// The subscription of a run from the machine settings, the project's
/// section and the run's own request; `None` for a run that does not
/// notify. A run's own request always comes back, even when no transition
/// is left once narrowed: the caller refuses that instead of dropping it.
pub fn effective(
    machine: &NotifySettings,
    project: &ProjectNotify,
    request: Option<&NotifyRequest>,
) -> Option<Effective> {
    let narrowed = || match &project.on {
        Some(only) => machine
            .on
            .iter()
            .copied()
            .filter(|t| only.contains(t))
            .collect(),
        None => machine.on.clone(),
    };
    let (target, on, source) = match request {
        Some(NotifyRequest::Off) => return None,
        Some(NotifyRequest::On { target, on }) => (
            target.clone().or_else(|| machine.target.clone()),
            on.clone().unwrap_or_else(narrowed),
            NotifySource::Run,
        ),
        None => {
            if machine.default == Toggle::Off || project.enabled == Some(false) {
                return None;
            }
            let source = if project.on.is_some() {
                NotifySource::Project
            } else {
                NotifySource::Machine
            };
            (machine.target.clone(), narrowed(), source)
        }
    };
    let on = Transition::normalized(&on);
    (!on.is_empty() || source.is_explicit()).then_some(Effective { target, on, source })
}
