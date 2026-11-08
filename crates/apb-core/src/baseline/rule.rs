//! The comparison of an observation's before and after values under its
//! rule, and the check of one value with no before value (a follow-up).
//! Pure: it reads nothing and journals nothing.
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
    /// A list holds this scalar among its items (compared as the item text
    /// a list keeps, [`Scalar::as_item`]).
    Contains(Scalar),
}

impl Rule {
    /// The rule as journaled: `equals 0`, `at_most 3`, `change no_new`,
    /// `empty`, `contains \`bug\``.
    pub fn describe(&self) -> String {
        match self {
            Rule::Equals(s) => format!("equals {s}"),
            Rule::AtMost(n) => format!("at_most {}", format_number(*n)),
            Rule::AtLeast(n) => format!("at_least {}", format_number(*n)),
            Rule::Change(c) => format!("change {}", c.as_str()),
            Rule::Empty => "empty".into(),
            Rule::Contains(s) => format!("contains {s}"),
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
                Rule::Empty
                    | Rule::Contains(_)
                    | Rule::Change(Change::NoNew | Change::Shrink | Change::Unchanged)
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
    /// An unknown comparison with the reason: the one constructor a
    /// follow-up read uses for a rule its block does not state.
    pub fn unknown(detail: impl Into<String>) -> Self {
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

/// One observation checked under `rule` with no before value (a
/// follow-up): `equals`, `at_most`, `at_least`, `empty` and `contains` only;
/// a `change` rule is unknown. `regressed` is always `None`; `detail` is
/// the observed value as people read it. An unknown verdict keeps its
/// reason as the detail instead: the observation's own, or why the rule
/// does not apply.
pub fn check(observation: &Observation, rule: &Rule) -> Comparison {
    let value = match observation {
        Observation::Unknown { reason } => return Comparison::unknown(reason.clone()),
        Observation::Value { value } => value,
    };
    if let Rule::Change(_) = rule {
        return Comparison::unknown(format!(
            "rule `{}` needs a before value, and a follow-up has none",
            rule.describe()
        ));
    }
    // An absolute rule reads only the after side, so the value as its own
    // before gives the value's verdict.
    let c = compare(Some(observation), observation, rule);
    if c.verdict == Verdict::Unknown {
        return c;
    }
    Comparison {
        verdict: c.verdict,
        regressed: None,
        detail: Some(value.to_string()),
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
        Rule::Contains(s) => {
            let item = s.as_item();
            let (was, is) = (b.contains(&item), a.contains(&item));
            (is, was && !is)
        }
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
    fn contains_is_met_only_when_the_after_list_holds_the_item() {
        let rule = Rule::Contains(Scalar::Text("bug".into()));
        let c = compare(Some(&list(&["a"])), &list(&["a", "bug"]), &rule);
        assert_eq!((c.verdict, c.regressed), (Verdict::Met, Some(false)));
        let c = compare(Some(&list(&["bug"])), &list(&["a"]), &rule);
        assert_eq!((c.verdict, c.regressed), (Verdict::NotMet, Some(true)));
        let c = compare(Some(&list(&[])), &list(&["bugs"]), &rule);
        assert_eq!((c.verdict, c.regressed), (Verdict::NotMet, Some(false)));
        let numeric = Rule::Contains(Scalar::Number(42.0));
        assert_eq!(
            compare(Some(&list(&[])), &list(&["42"]), &numeric).verdict,
            Verdict::Met
        );
        let unknown = Observation::unknown("timeout");
        let c = compare(Some(&list(&["bug"])), &unknown, &rule);
        assert_eq!((c.verdict, c.regressed), (Verdict::Unknown, None));
        let text = Observation::value(ObservedValue::Text("a bug".into()));
        assert_eq!(
            compare(Some(&text), &text, &rule).verdict,
            Verdict::Unknown,
            "a field is never searched for a substring"
        );
        assert_eq!(rule.describe(), "contains `bug`");
    }

    #[test]
    fn rules_fit_only_their_observation_types() {
        // `contains` on a list and on a number is the V64 test's; a field
        // is checked only here.
        let contains = Rule::Contains(Scalar::Text("x".into()));
        assert!(contains.misfit(ObservationType::Field).is_some());
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

    fn field(s: &str) -> Observation {
        Observation::value(ObservedValue::Text(s.into()))
    }

    fn flag(b: bool) -> Observation {
        Observation::value(ObservedValue::Bool(b))
    }

    /// One value under each absolute rule, on the observation types it
    /// fits: the verdict is the value's, never a regression, and the detail
    /// is the value as people read it.
    #[test]
    fn check_meets_and_misses_each_absolute_rule() {
        let zero = Rule::Equals(Scalar::Number(0.0));
        let won = Rule::Equals(Scalar::Text("won".into()));
        let yes = Rule::Equals(Scalar::Bool(true));
        let bug = Rule::Contains(Scalar::Text("bug".into()));
        let cases = [
            // number
            (num(0.0), &zero, Verdict::Met, "0"),
            (num(37.0), &zero, Verdict::NotMet, "37"),
            (num(3.0), &Rule::AtMost(3.0), Verdict::Met, "3"),
            (num(4.0), &Rule::AtMost(3.0), Verdict::NotMet, "4"),
            (num(5.0), &Rule::AtLeast(5.0), Verdict::Met, "5"),
            (num(4.5), &Rule::AtLeast(5.0), Verdict::NotMet, "4.5"),
            // list
            (list(&[]), &Rule::Empty, Verdict::Met, "0 item(s)"),
            (list(&["a"]), &Rule::Empty, Verdict::NotMet, "1 item(s)"),
            (list(&["a", "bug"]), &bug, Verdict::Met, "2 item(s)"),
            (list(&["bugs"]), &bug, Verdict::NotMet, "1 item(s)"),
            // field
            (field("won"), &won, Verdict::Met, "`won`"),
            (field("lost"), &won, Verdict::NotMet, "`lost`"),
            (flag(true), &yes, Verdict::Met, "true"),
            (flag(false), &yes, Verdict::NotMet, "false"),
        ];
        for (observation, rule, verdict, detail) in cases {
            let c = check(&observation, rule);
            let at = format!("{} on {observation:?}", rule.describe());
            assert_eq!(c.verdict, verdict, "{at}");
            assert_eq!(c.regressed, None, "{at}");
            assert_eq!(c.detail.as_deref(), Some(detail), "{at}");
        }
    }

    /// A rule on a type it does not fit (V64 refuses it) has no answer, and
    /// says why.
    #[test]
    fn check_of_a_rule_that_does_not_fit_is_unknown() {
        let cases = [
            (list(&["a"]), Rule::Equals(Scalar::Text("a".into()))),
            (list(&["a"]), Rule::AtMost(3.0)),
            (field("won"), Rule::AtLeast(1.0)),
            (num(0.0), Rule::Empty),
            (field("a bug"), Rule::Contains(Scalar::Text("bug".into()))),
        ];
        for (observation, rule) in cases {
            let c = check(&observation, &rule);
            let at = format!("{} on {observation:?}", rule.describe());
            assert_eq!((c.verdict, c.regressed), (Verdict::Unknown, None), "{at}");
            assert!(
                c.detail.as_deref().unwrap().contains("does not apply"),
                "{at}: {c:?}"
            );
        }
    }

    /// A follow-up has no before value, so no `change` rule has an answer.
    #[test]
    fn check_of_a_change_rule_is_unknown() {
        for (observation, change) in [
            (num(3.0), Change::Decrease),
            (num(3.0), Change::NoIncrease),
            (list(&["a"]), Change::NoNew),
            (list(&["a"]), Change::Shrink),
            (field("won"), Change::Unchanged),
        ] {
            let rule = Rule::Change(change);
            let c = check(&observation, &rule);
            assert_eq!(
                (c.verdict, c.regressed),
                (Verdict::Unknown, None),
                "{}",
                rule.describe()
            );
            assert!(
                c.detail.as_deref().unwrap().contains("before value"),
                "{c:?}"
            );
        }
    }

    /// An unknown observation is unknown under every rule, with its reason.
    #[test]
    fn check_of_an_unknown_observation_is_unknown() {
        let unknown = Observation::unknown("timeout");
        for rule in [
            Rule::Equals(Scalar::Number(0.0)),
            Rule::AtMost(3.0),
            Rule::Empty,
            Rule::Contains(Scalar::Text("bug".into())),
            Rule::Change(Change::Decrease),
        ] {
            let c = check(&unknown, &rule);
            assert_eq!(
                (c.verdict, c.regressed, c.detail.as_deref()),
                (Verdict::Unknown, None, Some("timeout")),
                "{}",
                rule.describe()
            );
        }
    }
}
