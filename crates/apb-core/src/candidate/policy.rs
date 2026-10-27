//! The supervisor policy a playbook declares under `supervisor.policy`,
//! read in one place:
//!
//! - `trial_candidates`: whether a start without an explicit version runs
//!   the candidate ([`TrialPolicy`]);
//! - `promote_supervisor_patches`: when a supervisor's patch becomes
//!   `current` ([`PromotePolicy`]), by successes or by quality.
//!
//! A value that does not read (an unknown word, an unknown key, a value
//! out of range) falls back to the default, and a quality gate that does
//! not read to `manual`; the validator reports that as V106 through
//! [`unreadable`], so the fallback is never silent.

use crate::quality::compare::{CompareMode, MIN_RUNS_FLOOR, QualityGate};
use crate::schema::Playbook;

pub const TRIAL_CANDIDATES: &str = "trial_candidates";
pub const PROMOTE_SUPERVISOR_PATCHES: &str = "promote_supervisor_patches";

/// `supervisor.policy.trial_candidates`: whether a run started without an
/// explicit version runs the candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrialPolicy {
    /// Never: the candidate runs only when asked for by version.
    Off,
    /// Every start without an explicit version runs it (the default).
    NextRun,
    /// That share of starts (0..=1) runs it, by a random draw per start.
    Share(f64),
}

impl TrialPolicy {
    /// Whether a start with the uniform draw `draw` (in `[0, 1)`) runs the
    /// candidate.
    pub fn admits(self, draw: f64) -> bool {
        match self {
            TrialPolicy::Off => false,
            TrialPolicy::NextRun => true,
            TrialPolicy::Share(p) => draw < p,
        }
    }
}

/// `supervisor.policy.promote_supervisor_patches`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PromotePolicy {
    OnSuccess,
    AfterNSuccesses(u32),
    Manual,
    Always,
    /// Promote a candidate by comparing its graded trials with `current`.
    Quality(QualityGate),
}

impl PromotePolicy {
    /// The trial successes that promote a candidate under a count policy;
    /// `None` when successes alone never promote (`manual`, `quality`).
    pub fn required_successes(self) -> Option<u32> {
        match self {
            PromotePolicy::Manual | PromotePolicy::Quality(_) => None,
            PromotePolicy::OnSuccess | PromotePolicy::Always => Some(1),
            PromotePolicy::AfterNSuccesses(n) => Some(n.max(1)),
        }
    }

    /// Whether an in-run patch the run migrated onto becomes `current` at
    /// the run's end. A workaround never does, and neither does a patch
    /// under a quality policy: one run is no comparison.
    pub fn should_promote(
        self,
        classification: &str,
        run_succeeded: bool,
        changed_nodes_succeeded: bool,
        prior_successes: u32,
    ) -> bool {
        if classification == "workaround" {
            return false;
        }
        match self {
            PromotePolicy::Manual | PromotePolicy::Quality(_) => false,
            PromotePolicy::Always => true,
            PromotePolicy::OnSuccess => run_succeeded && changed_nodes_succeeded,
            PromotePolicy::AfterNSuccesses(required) => {
                run_succeeded
                    && changed_nodes_succeeded
                    && prior_successes.saturating_add(1) >= required
            }
        }
    }
}

/// The value of `supervisor.policy.<key>`, if the playbook sets it.
pub fn policy_value<'a>(playbook: &'a Playbook, key: &str) -> Option<&'a yaml_serde::Value> {
    playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.policy.as_ref())
        .and_then(|p| p.get(key))
}

/// The keys of a mapping value, or an error naming the first key outside
/// `allowed` (a typo or a mix of two forms is never read silently).
fn only_keys(value: &yaml_serde::Value, allowed: &[&str]) -> Result<(), String> {
    let Some(map) = value.as_mapping() else {
        return Ok(());
    };
    for key in map.keys() {
        if !key.as_str().is_some_and(|name| allowed.contains(&name)) {
            let name = match key.as_str() {
                Some(name) => name.to_string(),
                None => yaml_serde::to_string(key)
                    .map(|k| k.trim().to_string())
                    .unwrap_or_else(|_| format!("{key:?}")),
            };
            return Err(format!(
                "unknown key `{name}` (expected {})",
                allowed
                    .iter()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

/// Reads `trial_candidates`: `off`, `next_run` or `{ share: <0..1> }`.
pub fn parse_trial(value: &yaml_serde::Value) -> Result<TrialPolicy, String> {
    if let Some(s) = value.as_str() {
        return match s {
            "off" => Ok(TrialPolicy::Off),
            "next_run" => Ok(TrialPolicy::NextRun),
            other => Err(format!("unknown value `{other}`")),
        };
    }
    only_keys(value, &["share"])?;
    match value.get("share").and_then(yaml_serde::Value::as_f64) {
        Some(p) if (0.0..=1.0).contains(&p) => Ok(TrialPolicy::Share(p)),
        Some(p) => Err(format!("`share` must be between 0 and 1, got {p}")),
        None => Err("expected `off`, `next_run` or `{ share: <0..1> }`".into()),
    }
}

/// The keys of the quality form.
const QUALITY_KEYS: [&str; 6] = [
    "when",
    "compare",
    "min_runs",
    "max_trials",
    "window",
    "margin",
];

/// Reads `promote_supervisor_patches`: `on_success`, `manual`, `always`,
/// `{ after_n_successes: <n> }` or `{ when: quality, compare, min_runs,
/// max_trials, window, margin }`.
pub fn parse_promote(value: &yaml_serde::Value) -> Result<PromotePolicy, String> {
    if let Some(s) = value.as_str() {
        return match s {
            "on_success" => Ok(PromotePolicy::OnSuccess),
            "manual" => Ok(PromotePolicy::Manual),
            "always" => Ok(PromotePolicy::Always),
            other => Err(format!("unknown value `{other}`")),
        };
    }
    if let Some(n) = value.get("after_n_successes") {
        only_keys(value, &["after_n_successes"])?;
        return n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .map(PromotePolicy::AfterNSuccesses)
            .ok_or_else(|| "`after_n_successes` must be a positive whole number".into());
    }
    match value.get("when").and_then(yaml_serde::Value::as_str) {
        Some("quality") => {
            only_keys(value, &QUALITY_KEYS)?;
            parse_quality(value).map(PromotePolicy::Quality)
        }
        Some(other) => Err(format!("unknown `when: {other}`")),
        None => Err("expected `on_success`, `manual`, `always`, `{ after_n_successes: <n> }` or `{ when: quality }`".into()),
    }
}

fn parse_quality(value: &yaml_serde::Value) -> Result<QualityGate, String> {
    let mut gate = QualityGate::default();
    if let Some(c) = value.get("compare") {
        gate.compare = match c.as_str() {
            Some("not_worse") => CompareMode::NotWorse,
            Some("better") => CompareMode::Better,
            _ => return Err("`compare` must be `not_worse` or `better`".into()),
        };
    }
    let count = |key: &str, default: u32| -> Result<u32, String> {
        match value.get(key) {
            None => Ok(default),
            Some(v) => v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("`{key}` must be a positive whole number")),
        }
    };
    gate.min_runs = count("min_runs", gate.min_runs)?;
    gate.max_trials = count("max_trials", gate.max_trials)?;
    gate.window = count("window", gate.window)?;
    if let Some(m) = value.get("margin") {
        gate.margin = m
            .as_f64()
            .filter(|m| (0.0..=1.0).contains(m))
            .ok_or("`margin` must be a number between 0 and 1")?;
    }
    if gate.min_runs < MIN_RUNS_FLOOR {
        return Err(format!("`min_runs` must be at least {MIN_RUNS_FLOOR}"));
    }
    // A gate that can never collect `min_runs` grades on a side never
    // decides anything.
    if gate.max_trials < gate.min_runs {
        return Err("`max_trials` must be at least `min_runs`".into());
    }
    if gate.window < gate.min_runs {
        return Err("`window` must be at least `min_runs`".into());
    }
    Ok(gate)
}

/// Whether a `promote_supervisor_patches` value asks for the quality gate,
/// read or not: a gate that does not read never relaxes into a policy that
/// promotes on successes.
pub fn asks_for_quality(value: &yaml_serde::Value) -> bool {
    value.get("when").is_some()
}

/// The playbook's `trial_candidates` (`next_run` when absent or unreadable).
pub fn trial_policy(playbook: &Playbook) -> TrialPolicy {
    policy_value(playbook, TRIAL_CANDIDATES)
        .and_then(|v| parse_trial(v).ok())
        .unwrap_or(TrialPolicy::NextRun)
}

/// The playbook's `promote_supervisor_patches`: `on_success` when absent
/// or unreadable, except that a quality gate that does not read is
/// `manual` (a person decides instead of a gate that was never asked for).
pub fn promote_policy(playbook: &Playbook) -> PromotePolicy {
    match policy_value(playbook, PROMOTE_SUPERVISOR_PATCHES) {
        None => PromotePolicy::OnSuccess,
        Some(v) => parse_promote(v).unwrap_or(if asks_for_quality(v) {
            PromotePolicy::Manual
        } else {
            PromotePolicy::OnSuccess
        }),
    }
}

/// Every policy key that is set but does not read, with why: each falls
/// back to its default.
pub fn unreadable(playbook: &Playbook) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(Err(e)) = policy_value(playbook, TRIAL_CANDIDATES).map(parse_trial) {
        out.push((TRIAL_CANDIDATES, e));
    }
    if let Some(Err(e)) = policy_value(playbook, PROMOTE_SUPERVISOR_PATCHES).map(parse_promote) {
        out.push((PROMOTE_SUPERVISOR_PATCHES, e));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_policy(key: &str, policy: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nsupervisor:\n  policy:\n    {key}: {policy}\nnodes: []\nedges: []\n"
        ))
        .unwrap()
    }

    #[test]
    fn the_trial_policy_reads_off_next_run_and_a_share() {
        let read = |p: &str| trial_policy(&with_policy(TRIAL_CANDIDATES, p));
        assert_eq!(read("off"), TrialPolicy::Off);
        assert_eq!(read("next_run"), TrialPolicy::NextRun);
        assert_eq!(read("{ share: 0.25 }"), TrialPolicy::Share(0.25));
        assert_eq!(read("sometimes"), TrialPolicy::NextRun);
        for bad in [
            "{ share: 7 }",
            "{ share: -0.2 }",
            "{ share: 0.5, sharre: 1 }",
        ] {
            assert_eq!(read(bad), TrialPolicy::NextRun, "{bad}");
            assert_eq!(
                unreadable(&with_policy(TRIAL_CANDIDATES, bad)).len(),
                1,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_share_admits_draws_below_it() {
        assert!(TrialPolicy::Share(0.3).admits(0.29));
        assert!(!TrialPolicy::Share(0.3).admits(0.3));
        assert!(!TrialPolicy::Off.admits(0.0));
    }
}
