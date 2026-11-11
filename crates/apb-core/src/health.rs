//! The `health:` block: the rules a watched routine is judged by across
//! its runs, and the one classification of how a run ended. The store and
//! the evaluation live in the engine ([`apb_engine::health`]).

use serde::{Deserialize, Serialize};

/// A playbook's health rules: what trips when a routine keeps failing, or
/// stops running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HealthBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_rate: Option<FailureRate>,
    /// A duration (`26h`): no successful run within it trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overdue: Option<String>,
    #[serde(default)]
    pub on_trip: OnTrip,
    /// Least time between two delivered trip notices of one rule; default
    /// `24h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<String>,
}

/// The `failure_rate` rule: over the newest `window` counted runs, at
/// least `at_least` of them failed, with at least `min_runs` counted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureRate {
    pub at_least: f64,
    pub window: u32,
    pub min_runs: u32,
}

/// What a tripped routine does: sends a notice (the default), or holds
/// the playbook against starts until a person clears it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTrip {
    #[default]
    Notify,
    Hold,
}

/// One health rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    ConsecutiveFailures,
    FailureRate,
    Overdue,
}

impl Rule {
    /// The machine-facing rule name.
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::ConsecutiveFailures => "consecutive_failures",
            Rule::FailureRate => "failure_rate",
            Rule::Overdue => "overdue",
        }
    }
}

/// The default cooldown between two delivered trip notices of one rule.
pub const DEFAULT_COOLDOWN: &str = "24h";

/// How a run counts for the health rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClass {
    Succeeded,
    /// `run_finished failed`, or aborted by the engine or the supervisor
    /// (a deadline, a quota, a budget), except a `superseded:` abort.
    Failed,
    /// Aborted by a person, superseded, or not ended: never counted.
    Ignored,
}

impl RunClass {
    /// `succeeded`, `failed` or `ignored`: the archive's `class` field.
    pub fn as_str(self) -> &'static str {
        match self {
            RunClass::Succeeded => "succeeded",
            RunClass::Failed => "failed",
            RunClass::Ignored => "ignored",
        }
    }

    /// The class an archived summary's `class` word stands for; `None`
    /// for a word this binary does not know.
    pub fn parse(s: &str) -> Option<RunClass> {
        match s {
            "succeeded" => Some(RunClass::Succeeded),
            "failed" => Some(RunClass::Failed),
            "ignored" => Some(RunClass::Ignored),
            _ => None,
        }
    }
}

/// The one classification of how a run ended: the fold's status word, who
/// wrote the terminal abort, and its full reason. A live run's fact and
/// an archived run's `class` field both come from it.
pub fn class_of(outcome: &str, aborted_by: Option<&str>, reason: Option<&str>) -> RunClass {
    match outcome {
        "succeeded" => RunClass::Succeeded,
        "failed" => RunClass::Failed,
        "aborted" => match aborted_by {
            // A person's stop is news to nobody: never counted. An
            // absent `by` is an older journal's shape, read the same way.
            None => RunClass::Ignored,
            Some("operator") => RunClass::Ignored,
            Some(_) => match reason {
                Some(r) if r.starts_with("superseded:") => RunClass::Ignored,
                _ => RunClass::Failed,
            },
        },
        // Not ended: never counted.
        _ => RunClass::Ignored,
    }
}

/// One run as a rule reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFact {
    pub run_id: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub class: RunClass,
}

/// One rule's verdict over the facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVerdict {
    pub rule: Rule,
    pub tripped: bool,
    pub detail: String,
}

/// The verdict of every rule `block` sets, in the order
/// `consecutive_failures`, `failure_rate`, `overdue`. Facts started before
/// `counted_since_ms` do not count; `overdue` measures from the newest
/// counted success's end, else from `overdue_from_ms`.
pub fn evaluate(
    block: &HealthBlock,
    facts: &[RunFact],
    counted_since_ms: Option<u64>,
    overdue_from_ms: u64,
    now_ms: u64,
) -> Vec<RuleVerdict> {
    let mut counted: Vec<&RunFact> = facts
        .iter()
        .filter(|f| f.class != RunClass::Ignored)
        .filter(|f| counted_since_ms.is_none_or(|since| f.started_ms >= since))
        .collect();
    counted.sort_by(|a, b| (b.started_ms, &b.run_id).cmp(&(a.started_ms, &a.run_id)));
    let mut out = Vec::new();
    if let Some(n) = block.consecutive_failures {
        let n = n as usize;
        let tripped =
            n > 0 && counted.len() >= n && counted[..n].iter().all(|f| f.class == RunClass::Failed);
        out.push(RuleVerdict {
            rule: Rule::ConsecutiveFailures,
            tripped,
            detail: if tripped {
                format!("{n} failed in a row, latest {}", counted[0].run_id)
            } else {
                String::new()
            },
        });
    }
    if let Some(fr) = &block.failure_rate {
        let window: Vec<&&RunFact> = counted.iter().take(fr.window as usize).collect();
        let failed = window
            .iter()
            .filter(|f| f.class == RunClass::Failed)
            .count();
        let total = window.len();
        let tripped = total > 0
            && total >= fr.min_runs as usize
            && (failed as f64) / (total as f64) >= fr.at_least;
        out.push(RuleVerdict {
            rule: Rule::FailureRate,
            tripped,
            detail: format!("{failed} of {total} failed"),
        });
    }
    if let Some(d) = block.overdue.as_deref() {
        let limit_ms = crate::duration::parse_duration_str(d)
            .map(|secs| secs.saturating_mul(1000))
            .unwrap_or(u64::MAX);
        let reference = counted
            .iter()
            .find(|f| f.class == RunClass::Succeeded)
            .map(|f| f.ended_ms.unwrap_or(f.started_ms))
            .unwrap_or(overdue_from_ms);
        let tripped = now_ms.saturating_sub(reference) > limit_ms;
        out.push(RuleVerdict {
            rule: Rule::Overdue,
            tripped,
            detail: format!("no success since {}", crate::clock::iso_utc(reference)),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3_600_000;

    fn fact(id: &str, started: u64, class: RunClass) -> RunFact {
        RunFact {
            run_id: id.into(),
            started_ms: started,
            ended_ms: Some(started + 10),
            class,
        }
    }

    fn block(yaml: &str) -> HealthBlock {
        yaml_serde::from_str(yaml).unwrap()
    }

    fn tripped(v: &[RuleVerdict], rule: Rule) -> bool {
        v.iter()
            .find(|r| r.rule == rule)
            .unwrap_or_else(|| panic!("no verdict for {rule:?}: {v:?}"))
            .tripped
    }

    #[test]
    fn consecutive_failures_trips_on_n() {
        let b = block("consecutive_failures: 2");
        let facts = [
            fact("p-1", 1, RunClass::Succeeded),
            fact("p-2", 2, RunClass::Failed),
            fact("p-3", 3, RunClass::Failed),
        ];
        let v = evaluate(&b, &facts, None, 0, 10);
        assert!(tripped(&v, Rule::ConsecutiveFailures));
        assert_eq!(v[0].detail, "2 failed in a row, latest p-3");
    }

    #[test]
    fn consecutive_failures_needs_n_in_a_row() {
        let b = block("consecutive_failures: 2");
        let facts = [
            fact("p-1", 1, RunClass::Failed),
            fact("p-2", 2, RunClass::Succeeded),
            fact("p-3", 3, RunClass::Failed),
        ];
        assert!(!tripped(
            &evaluate(&b, &facts, None, 0, 10),
            Rule::ConsecutiveFailures
        ));
        assert!(!tripped(
            &evaluate(&b, &facts[2..], None, 0, 10),
            Rule::ConsecutiveFailures
        ));
    }

    #[test]
    fn failure_rate_trips_at_the_threshold() {
        let b = block("failure_rate: { at_least: 0.5, window: 4, min_runs: 2 }");
        let facts = [
            fact("p-1", 1, RunClass::Failed),
            fact("p-2", 2, RunClass::Succeeded),
            fact("p-3", 3, RunClass::Failed),
            fact("p-4", 4, RunClass::Succeeded),
            // Outside the window of the newest four.
            fact("p-0", 0, RunClass::Succeeded),
        ];
        let v = evaluate(&b, &facts, None, 0, 10);
        assert!(tripped(&v, Rule::FailureRate));
        assert_eq!(v[0].detail, "2 of 4 failed");
    }

    #[test]
    fn failure_rate_waits_for_min_runs() {
        let b = block("failure_rate: { at_least: 0.5, window: 10, min_runs: 3 }");
        let facts = [
            fact("p-1", 1, RunClass::Failed),
            fact("p-2", 2, RunClass::Failed),
        ];
        assert!(!tripped(
            &evaluate(&b, &facts, None, 0, 10),
            Rule::FailureRate
        ));
    }

    #[test]
    fn overdue_measures_from_the_last_success() {
        let b = block("overdue: 26h");
        let facts = [
            fact("p-1", 0, RunClass::Succeeded),
            fact("p-2", 10 * H, RunClass::Failed),
        ];
        assert!(!tripped(
            &evaluate(&b, &facts, None, 0, 25 * H),
            Rule::Overdue
        ));
        let v = evaluate(&b, &facts, None, 0, 27 * H);
        assert!(tripped(&v, Rule::Overdue));
        assert_eq!(v[0].detail, "no success since 1970-01-01T00:00:00Z");
    }

    #[test]
    fn overdue_gives_grace_from_its_reference() {
        let b = block("overdue: 26h");
        assert!(!tripped(
            &evaluate(&b, &[], None, 100 * H, 120 * H),
            Rule::Overdue
        ));
        assert!(tripped(
            &evaluate(&b, &[], None, 100 * H, 127 * H),
            Rule::Overdue
        ));
    }

    #[test]
    fn without_counted_since_every_fact_counts() {
        let b = block("consecutive_failures: 3");
        let facts = [
            fact("p-1", 1, RunClass::Failed),
            fact("p-2", 2, RunClass::Failed),
            fact("p-3", 3, RunClass::Failed),
        ];
        // A first evaluation after the third run: all three count.
        assert!(tripped(
            &evaluate(&b, &facts, None, 3, 10),
            Rule::ConsecutiveFailures
        ));
    }

    #[test]
    fn facts_before_counted_since_do_not_count() {
        let b = block("consecutive_failures: 2\noverdue: 26h");
        let facts = [
            fact("p-1", 1, RunClass::Succeeded),
            fact("p-2", 2, RunClass::Failed),
            fact("p-3", 3, RunClass::Failed),
        ];
        let v = evaluate(&b, &facts, Some(3), 3, 10);
        assert!(!tripped(&v, Rule::ConsecutiveFailures));
        // The success before the clear is no reference: overdue measures
        // from the clear.
        assert!(!tripped(
            &evaluate(&b, &facts, Some(3), 3, 25 * H),
            Rule::Overdue
        ));
        assert!(tripped(
            &evaluate(&b, &facts, Some(H), H, 28 * H),
            Rule::Overdue
        ));
    }

    #[test]
    fn ignored_facts_do_not_break_a_streak() {
        let b = block("consecutive_failures: 2");
        let facts = [
            fact("p-1", 1, RunClass::Failed),
            fact("p-2", 2, RunClass::Ignored),
            fact("p-3", 3, RunClass::Failed),
            fact("p-4", 4, RunClass::Ignored),
        ];
        let v = evaluate(&b, &facts, None, 0, 10);
        assert!(tripped(&v, Rule::ConsecutiveFailures));
        assert_eq!(v[0].detail, "2 failed in a row, latest p-3");
    }

    #[test]
    fn a_person_abort_is_ignored() {
        assert_eq!(
            class_of("aborted", Some("operator"), Some("stop requested")),
            RunClass::Ignored
        );
    }

    #[test]
    fn a_superseded_abort_is_ignored() {
        assert_eq!(
            class_of(
                "aborted",
                Some("engine"),
                Some("superseded: run c started for key k")
            ),
            RunClass::Ignored
        );
    }

    #[test]
    fn a_deadline_abort_is_a_failure() {
        assert_eq!(
            class_of("aborted", Some("engine"), Some("deadline_exceeded")),
            RunClass::Failed
        );
    }

    #[test]
    fn an_unended_run_is_ignored() {
        assert_eq!(class_of("running", None, None), RunClass::Ignored);
    }

    #[test]
    fn an_abort_without_by_is_ignored() {
        assert_eq!(
            class_of("aborted", None, Some("anything")),
            RunClass::Ignored
        );
    }

    #[test]
    fn class_words_round_trip() {
        for c in [RunClass::Succeeded, RunClass::Failed, RunClass::Ignored] {
            assert_eq!(RunClass::parse(c.as_str()), Some(c));
        }
        assert_eq!(RunClass::parse("other"), None);
    }
}
