//! A subscription before it is in force: the check every launch surface
//! runs before a start, the `notify_set` a run journals at start, what a
//! sub-playbook inherits, and a person's change on a live run.
//!
//! The check loads the live connector, requires the function to carry
//! notifications (`people.to` and `idempotency`), and requires the
//! connector and the account to be approved; the plan pins both digests.
//! A person who asked for notifications and cannot have them is refused
//! (`notify_unavailable`); a machine default that cannot be used only
//! warns, and the run journals the problem.

use std::path::Path;

use apb_core::notify::{NotifyRequest, NotifySource, NotifyTarget, Transition};

use super::NotifyState;
use super::record::NotifyTargetRecord;
use crate::connector::call::LiveTarget;
use crate::error::EngineError;
use crate::event::{EventLog, EventPayload};
use crate::pause::kind::PauseBy;
use crate::state::RunState;

/// What a refused start's error says first, so every launch surface can
/// answer it as a notify refusal.
pub const REFUSED_START: &str = "notifications were asked for but cannot be sent";

/// A subscription ready to journal: the target with its pinned digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyPlan {
    pub target: NotifyTargetRecord,
    pub on: Vec<Transition>,
    pub source: NotifySource,
}

/// What a launch surface checks before it starts a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifyCheck {
    /// The subscription the run will have; `None` without one.
    pub plan: Option<NotifyPlan>,
    /// Why the start must be refused: the person asked for notifications
    /// and the target cannot carry them.
    pub refusal: Option<String>,
    /// Why the machine default will not notify for this run.
    pub warnings: Vec<String>,
    /// The source of a default that could not be used, for its journal
    /// record.
    failed: Option<NotifySource>,
}

impl NotifyCheck {
    /// The `notify_set` the run journals: the plan, or a default's problem.
    /// `None` when the run has no subscription and nothing to say about it.
    pub fn event(&self) -> Option<EventPayload> {
        if let Some(plan) = &self.plan {
            return Some(set_event(
                Some(plan.target.clone()),
                &plan.on,
                plan.source,
                None,
                None,
            ));
        }
        let source = self.failed?;
        Some(set_event(
            None,
            &[],
            source,
            None,
            self.warnings.first().cloned(),
        ))
    }
}

/// The check of a run's subscription in the workspace at `root`, from the
/// machine config, the project config and the run's `request` (`None`:
/// the run asks for nothing).
pub fn check(root: &Path, request: Option<&NotifyRequest>) -> NotifyCheck {
    // A run that asked for none has nothing to check, whatever the configs.
    if matches!(request, Some(NotifyRequest::Off)) {
        return NotifyCheck::default();
    }
    let explicit = matches!(request, Some(NotifyRequest::On { .. }));
    let eff = match settings(root, request) {
        Ok(Some(eff)) => eff,
        Ok(None) => return NotifyCheck::default(),
        // Settings that cannot even be read are journaled as the source
        // whose file failed.
        Err((problem, source)) => return unavailable(problem, explicit, Some(source)),
    };
    let source = eff.source;
    match verify(root, eff) {
        Ok(plan) => NotifyCheck {
            plan: Some(plan),
            ..NotifyCheck::default()
        },
        Err(problem) => unavailable(problem, explicit || source.is_explicit(), Some(source)),
    }
}

/// The check of a subscription that cannot be used: a refusal for a person
/// who asked for it, a warning (and a journaled problem) for a default.
fn unavailable(problem: String, explicit: bool, source: Option<NotifySource>) -> NotifyCheck {
    if explicit {
        return NotifyCheck {
            refusal: Some(problem),
            ..NotifyCheck::default()
        };
    }
    NotifyCheck {
        warnings: vec![format!("notifications are off for this run: {problem}")],
        failed: source,
        ..NotifyCheck::default()
    }
}

/// The effective subscription from the three configs; an error names the
/// source whose settings did not read.
fn settings(
    root: &Path,
    request: Option<&NotifyRequest>,
) -> Result<Option<apb_core::notify::Effective>, (String, NotifySource)> {
    if let Some(r) = request {
        r.validate().map_err(|e| (e, NotifySource::Run))?;
    }
    let machine = apb_core::config::GlobalConfig::load().map_err(|e| (e, NotifySource::Machine))?;
    let project = apb_core::config::project_notify(root).map_err(|e| (e, NotifySource::Project))?;
    Ok(apb_core::notify::effective(
        &machine.notify,
        &project,
        request,
    ))
}

/// The plan of `eff`: its target loads, can carry notifications and is
/// approved; the plan pins the digests it was approved with.
fn verify(root: &Path, eff: apb_core::notify::Effective) -> Result<NotifyPlan, String> {
    let target = eff.target.ok_or_else(|| {
        "no notify target is configured (notify.target in config.yaml)".to_string()
    })?;
    target.validate()?;
    if eff.on.is_empty() {
        return Err(
            "no transition is left to notify about once the project's notify.on narrows the request"
                .to_string(),
        );
    }
    let live = LiveTarget::load(
        root,
        &target.connector,
        Some(&target.account),
        &target.function,
    )
    .map_err(|e| e.message)?;
    if !live.function().carries_notices() {
        return Err(format!(
            "`{}.{}` cannot carry notifications: it needs a `people:` block that names `to` and every arg its args_schema requires, and `idempotency`",
            target.connector, target.function
        ));
    }
    live.require_trust().map_err(|e| e.message)?;
    let pins = live.pins();
    Ok(NotifyPlan {
        target: record(&target, pins.connector, pins.account),
        on: eff.on,
        source: eff.source,
    })
}

fn record(
    t: &NotifyTarget,
    connector_digest: String,
    account_digest: String,
) -> NotifyTargetRecord {
    NotifyTargetRecord {
        connector: t.connector.clone(),
        account: t.account.clone(),
        function: t.function.clone(),
        to: t.to.clone(),
        connector_digest,
        account_digest,
    }
}

fn set_event(
    target: Option<NotifyTargetRecord>,
    on: &[Transition],
    source: NotifySource,
    by: Option<PauseBy>,
    problem: Option<String>,
) -> EventPayload {
    EventPayload::NotifySet {
        target,
        on: on.iter().map(|t| t.as_str().to_string()).collect(),
        source: source.as_str().to_string(),
        by,
        problem,
    }
}

/// The `notify_set` a sub-playbook journals at its start: its parent's
/// subscription, narrowed to reviews and questions, with the same pinned
/// target. `None` when the parent notifies about neither.
pub(crate) fn inherited(parent_dir: &Path) -> Option<EventPayload> {
    let events = crate::event::read_all_lossy_tail(parent_dir).ok()?;
    let active = NotifyState::fold(&events).active?;
    let on = super::inheritable(&active.on);
    (!on.is_empty()).then(|| set_event(Some(active.target), &on, NotifySource::Parent, None, None))
}

/// A person's change of the subscription of live run `run_id`: `Off`
/// turns it off; `On` checks the target like a start does and journals
/// it. A target or a transition list left out keeps the run's current
/// one, else the default. A refused change journals nothing and comes back
/// in the check.
pub fn set(
    root: &Path,
    run_id: &str,
    request: &NotifyRequest,
    by: ChangedBy,
) -> Result<NotifyCheck, SetError> {
    let not_found = || SetError::NotFound(format!("run `{run_id}`"));
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(not_found());
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    let events = crate::event::read_all_lossy_tail(&run_dir).map_err(|_| not_found())?;
    if events.is_empty() {
        return Err(not_found());
    }
    if RunState::fold(&events).run_status.is_terminal() {
        return Err(SetError::Ended(format!(
            "run `{run_id}` has ended; its notifications can no longer change"
        )));
    }
    let (check, payload) = match request {
        NotifyRequest::Off => (
            NotifyCheck::default(),
            set_event(
                None,
                &[],
                NotifySource::Person,
                Some(PauseBy::Operator),
                None,
            ),
        ),
        NotifyRequest::On { target, on } => {
            let state = NotifyState::fold(&events);
            if by == ChangedBy::Host {
                host_bounds(root, &state, target.as_ref(), on.as_deref())?;
            }
            let current = state.active;
            let request = NotifyRequest::On {
                target: target
                    .clone()
                    .or_else(|| current.as_ref().map(|a| a.target.target())),
                on: on.clone().or_else(|| current.map(|a| a.on)),
            };
            let mut check = check(root, Some(&request));
            let Some(plan) = check.plan.as_mut() else {
                // An explicit request either plans or is refused; anything
                // else must not pass as a change that was made.
                if check.refusal.is_none() {
                    check.refusal = Some("the change names no subscription to set".to_string());
                }
                return Ok(check);
            };
            plan.source = NotifySource::Person;
            let payload = set_event(
                Some(plan.target.clone()),
                &plan.on,
                NotifySource::Person,
                Some(PauseBy::Operator),
                None,
            );
            (check, payload)
        }
    };
    EventLog::open_foreign(&run_dir)
        .and_then(|mut log| log.append(payload))
        .map_err(SetError::Journal)?;
    Ok(check)
}

/// Who changes a live run's subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedBy {
    /// The person, from a terminal outside any run or the dashboard: any
    /// target, any transitions.
    Person,
    /// A host on the person's behalf (MCP `run_notify`): on or off, and at
    /// most the transitions the person chose, never a target.
    Host,
}

/// A host's change, against the journal the change is made on: no target,
/// and no transition beyond the ones the run notifies about, or last
/// notified about, or else the default a plain "on" gets.
fn host_bounds(
    root: &Path,
    state: &NotifyState,
    target: Option<&NotifyTarget>,
    on: Option<&[Transition]>,
) -> Result<(), SetError> {
    if target.is_some() {
        return Err(SetError::NotAllowed(
            "a host does not choose where notifications go; the person sets a target in the dashboard or with `apb notify`".to_string(),
        ));
    }
    let Some(on) = on else {
        return Ok(());
    };
    let bound = match (&state.active, state.last_on.is_empty()) {
        (Some(a), _) => a.on.clone(),
        (None, false) => state.last_on.clone(),
        (None, true) => {
            let plain = NotifyRequest::On {
                target: None,
                on: None,
            };
            settings(root, Some(&plain))
                .ok()
                .flatten()
                .map(|e| e.on)
                .unwrap_or_default()
        }
    };
    match on.iter().find(|t| !bound.contains(t)) {
        Some(wider) => Err(SetError::NotAllowed(format!(
            "`{}` is not among the transitions the person chose for this run; a host only narrows them, the person widens them in the dashboard or with `apb notify`",
            wider.as_str()
        ))),
        None => Ok(()),
    }
}

/// Why a change of a live run's subscription was not made.
#[derive(Debug)]
pub enum SetError {
    /// No such run.
    NotFound(String),
    /// The run has ended.
    Ended(String),
    /// A host asked for what only the person may change.
    NotAllowed(String),
    /// The journal could not take the change (an unreadable journal, one a
    /// newer apb wrote, a failed write).
    Journal(EngineError),
}
