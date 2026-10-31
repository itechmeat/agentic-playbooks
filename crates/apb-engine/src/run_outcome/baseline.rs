//! The run's baseline check as the run surfaces show it: per check, the
//! before and after snapshots' status, and per observation both values,
//! the rule and the verdict the finish journaled. Read from the journal
//! only ([`crate::baseline::BaselineState`]); nothing here reads a source.

use apb_core::baseline::{BaselineCheck, BaselineSource, Observation, Verdict};
use apb_core::schema::Playbook;
use serde::Serialize;

use crate::baseline::{BaselineState, CheckState, Snapshot, project};
use crate::event::Event;

/// The verdict of an observation the finish has not compared yet.
pub const PENDING: &str = "pending";

/// One observation: both values, the rule and what the finish found.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservationView {
    pub id: String,
    /// The rule as journaled (`equals 0`, `change no_new`,
    /// `no_new_failures`).
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub before: Option<Observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub after: Option<Observation>,
    /// `37 -> 0`, the change as people read it.
    pub change: String,
    /// `met`, `not_met`, `unknown`, or `pending` before the finish.
    pub verdict: String,
    /// The after value is strictly worse than the before one; absent when
    /// either is unknown or not compared yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub regressed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}

/// One check of the playbook's `baseline_check`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BaselineCheckView {
    pub id: String,
    pub description: String,
    /// `connector`, `script` or `project_checks`.
    pub source: String,
    /// `observed`, `unknown` or `missed`; absent before it was taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub before_status: Option<String>,
    /// Why the before snapshot could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub before_reason: Option<String>,
    /// The run a continued run took its before snapshot from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub inherited_from: Option<String>,
    /// Absent until the finish took the after snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub after_status: Option<String>,
    pub observations: Vec<ObservationView>,
}

/// The run's baseline check; absent for a playbook without one.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunBaseline {
    /// `baseline_check.enforce`: anything but met fails the run.
    pub enforce: bool,
    pub checks: Vec<BaselineCheckView>,
    pub met: usize,
    pub not_met: usize,
    pub unknown: usize,
    /// Observations whose after value is worse than the before one.
    pub regressions: usize,
}

impl RunBaseline {
    /// Every observation of every check.
    pub fn observations(&self) -> impl Iterator<Item = (&BaselineCheckView, &ObservationView)> {
        self.checks
            .iter()
            .flat_map(|c| c.observations.iter().map(move |o| (c, o)))
    }

    /// Whether the finish compared the observations.
    pub fn compared(&self) -> bool {
        self.met + self.not_met + self.unknown > 0
    }
}

/// The baseline of the run's playbook snapshot with what `events`
/// journaled; `None` when the playbook declares no baseline check.
pub fn baseline_from(playbook: Option<&Playbook>, events: &[Event]) -> Option<RunBaseline> {
    let playbook = playbook?;
    let block = playbook.baseline_check.as_ref()?;
    let state = BaselineState::fold(playbook, events);
    let empty = CheckState::default();
    let checks: Vec<BaselineCheckView> = block
        .checks
        .iter()
        .map(|check| check_view(check, state.check(&check.id).unwrap_or(&empty)))
        .collect();
    let mut out = RunBaseline {
        enforce: block.enforce,
        checks: Vec::new(),
        met: 0,
        not_met: 0,
        unknown: 0,
        regressions: 0,
    };
    for o in checks.iter().flat_map(|c| &c.observations) {
        match o.verdict.as_str() {
            v if v == Verdict::Met.as_str() => out.met += 1,
            v if v == Verdict::NotMet.as_str() => out.not_met += 1,
            v if v == Verdict::Unknown.as_str() => out.unknown += 1,
            _ => {}
        }
        out.regressions += usize::from(o.regressed == Some(true));
    }
    out.checks = checks;
    Some(out)
}

fn check_view(check: &BaselineCheck, state: &CheckState) -> BaselineCheckView {
    let before = state.before.as_ref();
    let observations = if state.comparisons.is_empty() {
        pending_rows(check, before)
    } else {
        state
            .comparisons
            .iter()
            .map(|c| ObservationView {
                id: c.observation.clone(),
                rule: c.rule.clone(),
                change: change_text(c.before.as_ref(), c.after.as_ref()),
                before: c.before.clone(),
                after: c.after.clone(),
                verdict: c.verdict.clone(),
                regressed: c.regressed,
                detail: c.detail.clone(),
            })
            .collect()
    };
    BaselineCheckView {
        id: check.id.clone(),
        description: check.description.clone(),
        source: check.source().map_or("", |s| s.kind()).to_string(),
        before_status: before.map(|b| b.status.clone()),
        before_reason: before.and_then(|b| b.reason.clone()),
        inherited_from: before.and_then(|b| b.inherited_from.clone()),
        after_status: state.after.as_ref().map(|a| a.status.clone()),
        observations,
    }
}

/// The observations the finish has not compared yet, with their before
/// values when the before snapshot is taken.
fn pending_rows(check: &BaselineCheck, before: Option<&Snapshot>) -> Vec<ObservationView> {
    let row = |id: String, rule: String, key: &str| {
        let b = before.map(|b| b.observation(key));
        ObservationView {
            change: change_text(b.as_ref(), None),
            id,
            rule,
            before: b,
            after: None,
            verdict: PENDING.to_string(),
            regressed: None,
            detail: None,
        }
    };
    match check.source() {
        Some(BaselineSource::ProjectChecks(_)) => before
            .map(|b| b.commands.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(|cmd| {
                row(
                    cmd.name.clone(),
                    project::RULE.to_string(),
                    &project::status_key(cmd),
                )
            })
            .collect(),
        _ => check
            .observe
            .iter()
            .map(|o| {
                let rule = o.expect.rule().map(|r| r.describe()).unwrap_or_default();
                row(o.id.clone(), rule, &o.id)
            })
            .collect(),
    }
}

/// `37 -> 0`; `unknown` for a side that could not be read and `not read
/// yet` for one that was not taken.
pub fn change_text(before: Option<&Observation>, after: Option<&Observation>) -> String {
    let side = |o: Option<&Observation>| match o {
        Some(Observation::Value { value }) => value.to_string(),
        Some(Observation::Unknown { .. }) => "unknown".to_string(),
        None => "not read yet".to_string(),
    };
    format!("{} -> {}", side(before), side(after))
}

/// `unowned/count: 37 -> 0, equals 0: met`, the one text line of an
/// observation.
pub fn observation_line(check: &BaselineCheckView, o: &ObservationView) -> String {
    let mut line = format!("{}/{}: {}", check.id, o.id, o.change);
    if !o.rule.is_empty() {
        line.push_str(&format!(", {}", o.rule));
    }
    line.push_str(&format!(": {}", o.verdict.replace('_', " ")));
    if o.regressed == Some(true) {
        line.push_str(", worse than before");
    }
    if let Some(d) = o.detail.as_deref().filter(|d| !d.is_empty()) {
        line.push_str(&format!(" ({d})"));
    }
    line
}
