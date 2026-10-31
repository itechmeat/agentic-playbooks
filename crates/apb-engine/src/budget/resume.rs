//! What a resume may change about a run's budget, and when a resume is
//! refused because of it.
//!
//! - A plain resume of a run its budget paused is refused until a person
//!   raises or sets the budget, changes `on_no_estimate`, or lets the paused
//!   node run once (`once`); the refusal names what was needed and left.
//! - Only a person changes a budget or resumes a budget pause: a supervisor
//!   or the engine is refused.
//! - A raise adds to the limit in force; a set replaces it; a set and a
//!   raise of the same dimension are refused. Spent is never reset: it is
//!   always derived from the journal.
//! - A budget may be set on a run that had none; its estimate is then
//!   computed if the run has no snapshot.
//! - A running run's budget is not changed: pause it first.
//!
//! A change is journaled only once the resume is admitted: [`check_resume`]
//! refuses up front and writes nothing, and the resume journals the
//! [`resume_events`] under the run's event lock right before its
//! `run_resumed`, under the same check that the run's lifecycle is the one
//! the resume planned against. A refused resume (environment drift, a
//! finished run, a busy workdir, a lost race) writes nothing, and a change
//! is applied once per pause: a second submit of it for the same pause is
//! refused as a superseded resume (or because the run is running again).

use std::path::Path;

use apb_core::budget::{BudgetLimit, BudgetSource, OnNoEstimate};

use super::fold::{BudgetFold, GatePause};
use super::kind::{BudgetSpent, GateBasis, GateDecision};
use crate::error::EngineError;
use crate::event::{Event, EventPayload, read_all};
use crate::pause::PauseBy;
use crate::run_estimate::{EstimateTarget, snapshot};

/// The prefix of the refusal of a plain resume of a budget pause, so a
/// surface can tell it from other conflicts.
pub const BUDGET_PAUSED: &str = "budget_paused";

/// The budget changes a resume asks for. Every field is optional; an empty
/// request changes nothing. Serialized only to hand it to a detached
/// driver.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BudgetResume {
    /// Tokens added to the token limit in force.
    pub raise_tokens: Option<u64>,
    /// Dollars added to the dollar limit in force.
    pub raise_usd: Option<f64>,
    /// A new token limit.
    pub tokens: Option<u64>,
    /// A new dollar limit.
    pub usd: Option<f64>,
    pub on_no_estimate: Option<OnNoEstimate>,
    /// Let the node the budget paused before run once over it.
    pub once: bool,
}

impl BudgetResume {
    pub fn is_empty(&self) -> bool {
        *self == BudgetResume::default()
    }

    fn changes_budget(&self) -> bool {
        self.raise_tokens.is_some()
            || self.raise_usd.is_some()
            || self.tokens.is_some()
            || self.usd.is_some()
            || self.on_no_estimate.is_some()
    }
}

fn invalid(msg: &str) -> EngineError {
    EngineError::Invalid(msg.to_string())
}

/// The limit a resume leaves in force, or why it cannot.
fn new_limit(current: Option<&BudgetFold>, req: &BudgetResume) -> Result<BudgetLimit, EngineError> {
    if req.tokens == Some(0) || req.raise_tokens == Some(0) {
        return Err(invalid(
            "a token budget and a token raise must be greater than 0",
        ));
    }
    let positive = |v: Option<f64>| v.is_none_or(|x| x.is_finite() && x > 0.0);
    if !positive(req.usd) || !positive(req.raise_usd) {
        return Err(invalid(
            "a dollar budget and a dollar raise must be finite amounts greater than 0",
        ));
    }
    if (req.tokens.is_some() && req.raise_tokens.is_some())
        || (req.usd.is_some() && req.raise_usd.is_some())
    {
        return Err(invalid(
            "set a new limit or raise it, not both, for the same dimension",
        ));
    }
    let limit = current.map(|f| f.limit).unwrap_or_default();
    let tokens = match (req.tokens, req.raise_tokens) {
        (Some(t), _) => Some(t),
        (None, Some(r)) => Some(
            limit
                .tokens
                .ok_or_else(|| invalid("the run has no token limit to raise; set one instead"))?
                .saturating_add(r),
        ),
        (None, None) => limit.tokens,
    };
    let usd = match (req.usd, req.raise_usd) {
        (Some(u), _) => Some(u),
        (None, Some(r)) => Some(
            limit
                .usd
                .ok_or_else(|| invalid("the run has no dollar limit to raise; set one instead"))?
                + r,
        ),
        (None, None) => limit.usd,
    };
    if usd.is_some_and(|u| !u.is_finite()) {
        return Err(invalid("the dollar limit would not be a finite amount"));
    }
    let out = BudgetLimit { tokens, usd };
    if !out.is_limited() {
        return Err(invalid("a budget needs a token or a dollar limit"));
    }
    Ok(out)
}

fn paused_refusal(p: &GatePause) -> EngineError {
    EngineError::Conflict(format!(
        "{BUDGET_PAUSED}: the run is paused by its budget before `{}` ({}; needs {}, left {}); \
         resume it with a raised or new budget, another on_no_estimate, or once to let `{}` run once over it",
        p.node,
        p.basis.as_str(),
        p.need.unwrap_or_default().text(),
        p.remaining.unwrap_or_default().text(),
        p.node,
    ))
}

/// Refuses a resume of run `run_id` by `by` that its budget does not allow,
/// or whose budget change is invalid. Read-only: the change itself is
/// journaled by the resume, next to its `run_resumed` ([`resume_events`]).
pub fn check_resume(
    root: &Path,
    run_id: &str,
    req: Option<&BudgetResume>,
    by: PauseBy,
) -> Result<(), EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Ok(());
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Ok(());
    }
    let events = read_all(&run_dir)?;
    let req = req.filter(|r| !r.is_empty());
    if req.is_some() {
        let status = crate::state::RunState::fold(&events).run_status;
        if status != crate::state::RunStatus::Paused
            && crate::liveness::driver_alive(&run_dir, run_id) == Some(true)
        {
            return Err(EngineError::Conflict(
                "the run is running: its budget changes only at a resume; pause it first".into(),
            ));
        }
    }
    resume_events(&events, req, by).map(|_| ())
}

/// The budget events a resume by `by` journals right before its
/// `run_resumed`, over the journal as it stands under the run's event
/// lock: the new budget (`budget_set`) and the one-time waiver
/// (`budget_gate waived`) `req` asks for; or why the budget refuses it.
pub(crate) fn resume_events(
    events: &[Event],
    req: Option<&BudgetResume>,
    by: PauseBy,
) -> Result<Vec<EventPayload>, EngineError> {
    let req = req.filter(|r| !r.is_empty());
    let fold = BudgetFold::of(events);
    let paused = fold.as_ref().and_then(BudgetFold::unresolved_pause);
    let Some(req) = req else {
        return match paused {
            Some(p) if by == PauseBy::Operator => Err(paused_refusal(p)),
            Some(_) => Err(EngineError::Conflict(format!(
                "{BUDGET_PAUSED}: the run is paused by its budget, and a budget pause is resumed by a person"
            ))),
            None => Ok(Vec::new()),
        };
    };
    if by != PauseBy::Operator {
        return Err(EngineError::Conflict(
            "only a person may change a run's budget".into(),
        ));
    }
    let waive = match (req.once, fold.as_ref().and_then(|f| f.last_pause.as_ref())) {
        (false, _) => None,
        (true, Some(p)) if fold.as_ref().is_some_and(|f| f.pause_open) => Some(p.clone()),
        (true, _) => {
            return Err(invalid(
                "once applies only to a run its budget paused before a node",
            ));
        }
    };
    let mut out = Vec::new();
    if req.changes_budget() {
        let limit = new_limit(fold.as_ref(), req)?;
        let on_no_estimate = req
            .on_no_estimate
            .or(fold.as_ref().map(|f| f.on_no_estimate))
            .unwrap_or_default();
        out.push(EventPayload::BudgetSet {
            tokens: limit.tokens,
            usd: limit.usd,
            on_no_estimate,
            source: Some(BudgetSource::Resume),
            by: Some(by),
        });
    }
    if let Some(p) = waive {
        out.push(EventPayload::BudgetGate {
            node: p.node,
            batch: p.batch,
            items: p.items,
            decision: GateDecision::Waived,
            basis: GateBasis::Operator,
            need: p.need,
            remaining: p.remaining,
            spent: BudgetSpent::default(),
            by: Some(by),
        });
    }
    Ok(out)
}

/// Computes and writes the estimate of the run at `run_dir` (under project
/// `root`) when it has none: a budget set at resume on a run started
/// without one. Best effort: without a snapshot the gate treats every node
/// as unestimated.
pub(crate) fn ensure_estimate(root: &Path, run_dir: &Path) {
    if snapshot::read(run_dir).is_some() {
        return;
    }
    let Some(playbook) = crate::legacy_snapshot::load_run_playbook(run_dir) else {
        return;
    };
    let run_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let manifest = crate::manifest::read(run_dir).ok().flatten();
    let target = EstimateTarget::from_manifest(&playbook, manifest.as_ref(), run_id);
    let _ = snapshot::write(
        run_dir,
        &crate::run_estimate::estimate(&[root.to_path_buf()], &target),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(usd: Option<f64>) -> BudgetFold {
        BudgetFold {
            limit: BudgetLimit {
                tokens: Some(100),
                usd,
            },
            on_no_estimate: OnNoEstimate::Pause,
            source: None,
            sets: 1,
            last_pause: None,
            pause_open: false,
            pause_resolved: false,
            waiver: None,
        }
    }

    #[test]
    fn a_dollar_raise_that_overflows_is_refused() {
        let raise = BudgetResume {
            raise_usd: Some(f64::MAX),
            ..BudgetResume::default()
        };
        let err = new_limit(Some(&fold(Some(f64::MAX))), &raise).unwrap_err();
        assert!(err.to_string().contains("finite"), "{err}");
        let fine = BudgetResume {
            raise_usd: Some(1.5),
            ..BudgetResume::default()
        };
        assert_eq!(
            new_limit(Some(&fold(Some(2.0))), &fine).unwrap().usd,
            Some(3.5)
        );
    }

    #[test]
    fn a_set_and_a_raise_of_one_dimension_are_refused() {
        let both = BudgetResume {
            tokens: Some(10),
            raise_tokens: Some(5),
            ..BudgetResume::default()
        };
        assert!(new_limit(Some(&fold(None)), &both).is_err());
        let across = BudgetResume {
            tokens: Some(10),
            raise_usd: Some(1.0),
            ..BudgetResume::default()
        };
        let l = new_limit(Some(&fold(Some(2.0))), &across).unwrap();
        assert_eq!((l.tokens, l.usd), (Some(10), Some(3.0)));
    }
}
