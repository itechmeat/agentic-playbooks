//! The comparison of an observation's before and after values under its
//! rule. Pure: it reads nothing and journals nothing.
//!
//! An unknown on either side, or a missing before snapshot, gives an
//! unknown verdict: nothing is guessed. `regressed` is true only when the
//! after value is strictly worse than the before value in the rule's
//! direction, and absent when either side is unknown.

use super::{Change, Observation, ObservationType, ObservedValue, Scalar, format_number};

/// The one rule an observation states.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    Equals(Scalar),
    AtMost(f64),
    AtLeast(f64),
    Change(Change),
    Empty,
}

impl Rule {
    /// The rule as journaled: `equals 0`, `at_most 3`, `change no_new`,
    /// `empty`.
    pub fn describe(&self) -> String {
        match self {
            Rule::Equals(s) => format!("equals {s}"),
            Rule::AtMost(n) => format!("at_most {}", format_number(*n)),
            Rule::AtLeast(n) => format!("at_least {}", format_number(*n)),
            Rule::Change(c) => format!("change {}", c.as_str()),
            Rule::Empty => "empty".into(),
        }
    }

    /// Why this rule does not fit an observation of `kind`, `None` when it
    /// does (validator V64).
    pub fn misfit(&self, kind: ObservationType) -> Option<String> {
        let fits = match kind {
            ObservationType::Number => matches!(
                self,
                Rule::Equals(Scalar::Number(_))
                    | Rule::AtMost(_)
                    | Rule::AtLeast(_)
                    | Rule::Change(
                        Change::Decrease
                            | Change::Increase
                            | Change::NoIncrease
                            | Change::NoDecrease
                            | Change::Unchanged
                    )
            ),
            ObservationType::List => matches!(
                self,
                Rule::Empty | Rule::Change(Change::NoNew | Change::Shrink | Change::Unchanged)
            ),
            ObservationType::Field => {
                matches!(self, Rule::Equals(_) | Rule::Change(Change::Unchanged))
            }
        };
        (!fits).then(|| {
            format!(
                "rule `{}` does not apply to a {} observation",
                self.describe(),
                kind.as_str()
            )
        })
    }
}

/// The verdict of one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Met,
    NotMet,
    Unknown,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Met => "met",
            Verdict::NotMet => "not_met",
            Verdict::Unknown => "unknown",
        }
    }
}

/// One observation's comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub verdict: Verdict,
    pub regressed: Option<bool>,
    pub detail: Option<String>,
}

impl Comparison {
    fn unknown(detail: impl Into<String>) -> Self {
        Comparison {
            verdict: Verdict::Unknown,
            regressed: None,
            detail: Some(detail.into()),
        }
    }

    fn of(met: bool, regressed: bool, detail: String) -> Self {
        Comparison {
            verdict: if met { Verdict::Met } else { Verdict::NotMet },
            regressed: Some(regressed),
            detail: Some(detail),
        }
    }
}

/// How many added items a list detail names.
const NAMED_ITEMS: usize = 5;

/// Compares `before` and `after` under `rule`.
pub fn compare(before: Option<&Observation>, after: &Observation, rule: &Rule) -> Comparison {
    let b = match before {
        None => return Comparison::unknown("there is no before snapshot"),
        Some(Observation::Unknown { reason }) => {
            return Comparison::unknown(format!("before: {reason}"));
        }
        Some(Observation::Value { value }) => value,
    };
    let a = match after {
        Observation::Unknown { reason } => return Comparison::unknown(format!("after: {reason}")),
        Observation::Value { value } => value,
    };
    match (b, a) {
        (ObservedValue::Number(b), ObservedValue::Number(a))
            if !matches!(rule, Rule::Equals(Scalar::Text(_) | Scalar::Bool(_))) =>
        {
            numbers(*b, *a, rule)
        }
        (ObservedValue::List(b), ObservedValue::List(a)) => lists(b, a, rule),
        (b, a) if !matches!(b, ObservedValue::List(_)) && !matches!(a, ObservedValue::List(_)) => {
            fields(b, a, rule)
        }
        _ => Comparison::unknown("the before and after values do not have the same type"),
    }
}

fn numbers(b: f64, a: f64, rule: &Rule) -> Comparison {
    let detail = format!("{} -> {}", format_number(b), format_number(a));
    let (met, regressed) = match rule {
        Rule::Equals(Scalar::Number(t)) => (a == *t, (a - t).abs() > (b - t).abs()),
        Rule::AtMost(t) => (a <= *t, a > b),
        Rule::AtLeast(t) => (a >= *t, a < b),
        Rule::Change(Change::Decrease) => (a < b, a > b),
        Rule::Change(Change::NoIncrease) => (a <= b, a > b),
        Rule::Change(Change::Increase) => (a > b, a < b),
        Rule::Change(Change::NoDecrease) => (a >= b, a < b),
        Rule::Change(Change::Unchanged) => (a == b, a != b),
        other => {
            return Comparison::unknown(format!(
                "rule `{}` does not apply to a number",
                other.describe()
            ));
        }
    };
    Comparison::of(met, regressed, detail)
}

fn lists(b: &[String], a: &[String], rule: &Rule) -> Comparison {
    let added: Vec<&String> = a.iter().filter(|x| !b.contains(x)).collect();
    let removed = b.iter().filter(|x| !a.contains(x)).count();
    let mut detail = format!("{} -> {} item(s)", b.len(), a.len());
    if !added.is_empty() {
        let named: Vec<&str> = added.iter().take(NAMED_ITEMS).map(|s| s.as_str()).collect();
        detail.push_str(&format!("; new: {}", named.join(", ")));
        if added.len() > NAMED_ITEMS {
            detail.push_str(&format!(" and {} more", added.len() - NAMED_ITEMS));
        }
    }
    let (met, regressed) = match rule {
        Rule::Change(Change::NoNew) => (added.is_empty(), !added.is_empty()),
        Rule::Change(Change::Shrink) => (added.is_empty() && removed > 0, !added.is_empty()),
        Rule::Change(Change::Unchanged) => {
            let same = added.is_empty() && removed == 0;
            (same, !same)
        }
        Rule::Empty => (a.is_empty(), a.len() > b.len()),
        other => {
            return Comparison::unknown(format!(
                "rule `{}` does not apply to a list",
                other.describe()
            ));
        }
    };
    Comparison::of(met, regressed, detail)
}

fn fields(b: &ObservedValue, a: &ObservedValue, rule: &Rule) -> Comparison {
    let detail = format!("{b} -> {a}");
    let (met, regressed) = match rule {
        Rule::Equals(s) => (s.matches(a), s.matches(b) && !s.matches(a)),
        Rule::Change(Change::Unchanged) => (a == b, a != b),
        other => {
            return Comparison::unknown(format!(
                "rule `{}` does not apply to a field",
                other.describe()
            ));
        }
    };
    Comparison::of(met, regressed, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(n: f64) -> Observation {
        Observation::value(ObservedValue::Number(n))
    }

    fn list(items: &[&str]) -> Observation {
        Observation::value(ObservedValue::List(
            items.iter().map(|s| s.to_string()).collect(),
        ))
    }

    #[test]
    fn unknown_on_either_side_or_no_before_is_unknown() {
        let rule = Rule::Equals(Scalar::Number(0.0));
        let unknown = Observation::unknown("timeout");
        for (before, after) in [
            (Some(&unknown), &num(0.0)),
            (Some(&num(3.0)), &unknown),
            (None, &num(0.0)),
        ] {
            let c = compare(before, after, &rule);
            assert_eq!(c.verdict, Verdict::Unknown, "{c:?}");
            assert_eq!(c.regressed, None);
        }
    }

    #[test]
    fn a_count_down_to_zero_meets_equals_zero() {
        let c = compare(
            Some(&num(37.0)),
            &num(0.0),
            &Rule::Equals(Scalar::Number(0.0)),
        );
        assert_eq!(c.verdict, Verdict::Met);
        assert_eq!(c.regressed, Some(false));
        assert_eq!(c.detail.as_deref(), Some("37 -> 0"));
    }

    #[test]
    fn a_partial_reduction_is_not_met_and_not_a_regression() {
        let c = compare(
            Some(&num(37.0)),
            &num(20.0),
            &Rule::Equals(Scalar::Number(0.0)),
        );
        assert_eq!(c.verdict, Verdict::NotMet);
        assert_eq!(c.regressed, Some(false));
    }

    #[test]
    fn decrease_that_grew_regressed() {
        let c = compare(
            Some(&num(37.0)),
            &num(40.0),
            &Rule::Change(Change::Decrease),
        );
        assert_eq!(c.verdict, Verdict::NotMet);
        assert_eq!(c.regressed, Some(true));
    }

    #[test]
    fn no_new_with_an_added_item_is_not_met_and_regressed() {
        let c = compare(
            Some(&list(&["a", "b"])),
            &list(&["b", "c"]),
            &Rule::Change(Change::NoNew),
        );
        assert_eq!(c.verdict, Verdict::NotMet);
        assert_eq!(c.regressed, Some(true));
        assert!(c.detail.unwrap().contains("new: c"));
        let c = compare(
            Some(&list(&["a", "b"])),
            &list(&["b"]),
            &Rule::Change(Change::NoNew),
        );
        assert_eq!(c.verdict, Verdict::Met);
    }

    #[test]
    fn a_field_equals_and_its_regression() {
        let rule = Rule::Equals(Scalar::Text("green".into()));
        let green = Observation::value(ObservedValue::Text("green".into()));
        let red = Observation::value(ObservedValue::Text("red".into()));
        assert_eq!(compare(Some(&red), &green, &rule).verdict, Verdict::Met);
        let c = compare(Some(&green), &red, &rule);
        assert_eq!((c.verdict, c.regressed), (Verdict::NotMet, Some(true)));
    }

    #[test]
    fn rules_fit_only_their_observation_types() {
        assert!(
            Rule::Change(Change::NoNew)
                .misfit(ObservationType::Number)
                .is_some()
        );
        assert!(Rule::Empty.misfit(ObservationType::List).is_none());
        assert!(Rule::AtMost(1.0).misfit(ObservationType::Field).is_some());
        assert!(
            Rule::Equals(Scalar::Text("x".into()))
                .misfit(ObservationType::Number)
                .is_some()
        );
    }
}
